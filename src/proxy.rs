use anyhow::{bail, Result};
use axum::http::HeaderMap;
use std::net::{IpAddr, SocketAddr};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    Direct,
    Nginx,
    Cloudflare,
}

impl ProxyMode {
    pub fn validate_bind(self, bind: &str) -> Result<()> {
        if self == Self::Cloudflare {
            let addr: SocketAddr = bind.parse()?;
            if !addr.ip().is_loopback() {
                bail!("Cloudflare mode requires a loopback bind and cloudflared on the same host/network namespace");
            }
        }
        Ok(())
    }

    pub fn client_ip(self, peer: SocketAddr, headers: &HeaderMap) -> Result<IpAddr, &'static str> {
        match self {
            Self::Direct => Ok(peer.ip()),
            Self::Nginx if peer.ip().is_loopback() => {
                if !headers.contains_key("x-real-ip") {
                    return Ok(peer.ip());
                }
                single_ip(headers, "x-real-ip")
            }
            Self::Nginx => Ok(peer.ip()),
            Self::Cloudflare => {
                if !peer.ip().is_loopback() {
                    return Err("非可信的隧道连接来源");
                }
                // Only trust Cloudflare's single authoritative visitor header. Never
                // fall back to X-Real-IP or an attacker-controlled X-Forwarded-For chain.
                single_ip(headers, "cf-connecting-ip")
            }
        }
    }
}

fn single_ip(headers: &HeaderMap, name: &str) -> Result<IpAddr, &'static str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next().ok_or("缺少代理访客 IP 请求头")?;
    if values.next().is_some() {
        return Err("代理访客 IP 请求头重复");
    }
    value
        .to_str()
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .ok_or("代理访客 IP 格式无效")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloudflare_requires_loopback_bind() {
        assert!(ProxyMode::Cloudflare.validate_bind("0.0.0.0:8787").is_err());
        assert!(ProxyMode::Cloudflare.validate_bind("[::]:8787").is_err());
        assert!(ProxyMode::Cloudflare
            .validate_bind("127.0.0.1:8787")
            .is_ok());
        assert!(ProxyMode::Cloudflare.validate_bind("[::1]:8787").is_ok());
    }

    #[test]
    fn cloudflare_accepts_ipv4_ipv6_and_ignores_spoofable_headers() {
        let peer = "127.0.0.1:40000".parse().unwrap();
        for ip in ["203.0.113.9", "2001:db8::1234"] {
            let mut headers = HeaderMap::new();
            headers.insert("CF-Connecting-IP", ip.parse().unwrap());
            headers.insert("x-real-ip", "192.0.2.99".parse().unwrap());
            headers.insert("x-forwarded-for", "192.0.2.88, 192.0.2.77".parse().unwrap());
            assert_eq!(
                ProxyMode::Cloudflare.client_ip(peer, &headers).unwrap(),
                ip.parse::<IpAddr>().unwrap()
            );
        }
    }

    #[test]
    fn cloudflare_rejects_missing_invalid_duplicate_and_remote_headers() {
        let peer = "127.0.0.1:40000".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", "203.0.113.1".parse().unwrap());
        assert!(ProxyMode::Cloudflare.client_ip(peer, &headers).is_err());
        for value in [
            "not-an-ip",
            "203.0.113.1, 203.0.113.2",
            "203.0.113.1:1234",
            "",
        ] {
            headers.insert("cf-connecting-ip", value.parse().unwrap());
            assert!(ProxyMode::Cloudflare.client_ip(peer, &headers).is_err());
        }
        headers.insert("cf-connecting-ip", "203.0.113.1".parse().unwrap());
        assert!(ProxyMode::Cloudflare
            .client_ip("192.0.2.10:40000".parse().unwrap(), &headers)
            .is_err());
        headers.append("cf-connecting-ip", "203.0.113.2".parse().unwrap());
        assert!(ProxyMode::Cloudflare.client_ip(peer, &headers).is_err());
    }

    #[test]
    fn direct_and_untrusted_nginx_peers_ignore_forged_ips() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", "203.0.113.1".parse().unwrap());
        headers.insert("x-real-ip", "203.0.113.2".parse().unwrap());
        for peer in ["127.0.0.1:42", "192.0.2.9:42"] {
            let peer: SocketAddr = peer.parse().unwrap();
            assert_eq!(
                ProxyMode::Direct.client_ip(peer, &headers).unwrap(),
                peer.ip()
            );
        }
        let peer: SocketAddr = "192.0.2.9:42".parse().unwrap();
        assert_eq!(
            ProxyMode::Nginx.client_ip(peer, &headers).unwrap(),
            peer.ip()
        );
    }
}
