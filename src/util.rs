use anyhow::{bail, Result};
use percent_encoding::percent_decode_str;
use rand::RngCore;
use sha2::{Digest, Sha256};

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
pub fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
pub fn token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn page(raw: &str) -> Result<String> {
    if raw.len() > 2048 || !raw.starts_with('/') || raw.starts_with("//") {
        bail!("invalid page path");
    }
    let path = raw.split(['?', '#']).next().unwrap_or("/");
    let decoded = percent_decode_str(path).decode_utf8()?;
    if decoded
        .chars()
        .any(|c| c.is_control() || matches!(c, '\\' | '?' | '#'))
        || decoded.starts_with("//")
        || decoded.split('/').any(|p| p == "." || p == "..")
    {
        bail!("invalid page path");
    }
    let segments: Vec<_> = decoded.split('/').filter(|p| !p.is_empty()).collect();
    if segments.is_empty() {
        Ok("/".into())
    } else {
        Ok(format!("/{}/", segments.join("/")))
    }
}
pub fn website(raw: &str) -> Result<String> {
    if raw.is_empty() {
        return Ok(String::new());
    }
    if raw.len() > 2048 {
        bail!("website is too long");
    }
    let u = url::Url::parse(raw)?;
    if !matches!(u.scheme(), "https" | "http")
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
    {
        bail!("website must use HTTP(S)");
    }
    Ok(u.to_string())
}
