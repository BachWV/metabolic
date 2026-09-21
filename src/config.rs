use crate::proxy::ProxyMode;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub bind: String,
    pub database: String,
    pub origins: Vec<String>,
    pub site_url: String,
    pub password_hash: String,
    pub cookie_secure: bool,
    pub proxy_mode: ProxyMode,
    #[serde(default)]
    pub smtp_host: String,
    #[serde(default = "smtp_port")]
    pub smtp_port: u16,
    #[serde(default)]
    pub smtp_user: String,
    #[serde(default)]
    pub smtp_password: String,
    #[serde(default)]
    pub mail_from: String,
    #[serde(default)]
    pub mail_to: String,
}
fn smtp_port() -> u16 {
    587
}

impl Config {
    pub fn from_file(path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .context("cannot locate YAML configuration file")?;
        let text = std::fs::read_to_string(&path).context("cannot read YAML configuration file")?;
        // Parser errors can include secret values. Expose only line and column.
        let mut config: Self = serde_yaml_ng::from_str(&text).map_err(|e| {
            if let Some(loc) = e.location() {
                anyhow::anyhow!(
                    "invalid YAML configuration at line {}, column {} (check fields and types)",
                    loc.line(),
                    loc.column()
                )
            } else {
                anyhow::anyhow!("invalid YAML configuration (check fields and types)")
            }
        })?;
        config.validate()?;
        let database = Path::new(&config.database);
        if database.is_relative() {
            config.database = path
                .parent()
                .expect("configuration file has parent")
                .join(database)
                .to_str()
                .context("database path must be UTF-8")?
                .to_owned();
        }
        Ok(config)
    }

    fn validate(&mut self) -> Result<()> {
        let site =
            url::Url::parse(&self.site_url).map_err(|_| anyhow::anyhow!("invalid site_url"))?;
        if !matches!(site.scheme(), "http" | "https")
            || site.host_str().is_none()
            || !site.username().is_empty()
            || site.password().is_some()
            || site.query().is_some()
            || site.fragment().is_some()
        {
            bail!("site_url must be an HTTP(S) URL without credentials, query or fragment");
        }
        self.site_url = self.site_url.trim_end_matches('/').to_owned();
        if self.origins.is_empty() {
            bail!("origins must not be empty");
        }
        for origin in &self.origins {
            let u = url::Url::parse(origin).map_err(|_| anyhow::anyhow!("invalid origin"))?;
            if !matches!(u.scheme(), "http" | "https")
                || u.origin().ascii_serialization() != *origin
            {
                bail!("origins must contain exact HTTP(S) origins, never *");
            }
        }
        let parsed = argon2::PasswordHash::new(&self.password_hash)
            .map_err(|_| anyhow::anyhow!("invalid password_hash"))?;
        if parsed.algorithm.as_str() != "argon2id" || parsed.salt.is_none() || parsed.hash.is_none()
        {
            bail!("password_hash must be a complete Argon2id hash");
        }
        argon2::Params::try_from(&parsed)
            .map_err(|_| anyhow::anyhow!("invalid Argon2id parameters"))?;
        self.bind
            .parse::<std::net::SocketAddr>()
            .context("bind must be an IP address and port")?;
        self.proxy_mode.validate_bind(&self.bind)?;
        if self.database.trim().is_empty() || self.database == ":memory:" {
            bail!("database must be a filesystem path");
        }
        if !self.smtp_host.is_empty() {
            if self.smtp_port == 0 {
                bail!("smtp_port must be nonzero");
            }
            self.mail_from
                .parse::<lettre::message::Mailbox>()
                .map_err(|_| anyhow::anyhow!("invalid mail_from"))?;
            self.mail_to
                .parse::<lettre::message::Mailbox>()
                .map_err(|_| anyhow::anyhow!("invalid mail_to"))?;
            if self.smtp_user.is_empty() || self.smtp_password.is_empty() {
                bail!("SMTP requires smtp_user and smtp_password");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::{password_hash::SaltString, Argon2, PasswordHasher};

    fn yaml() -> String {
        let password = rand::random::<[u8; 32]>();
        let hash = Argon2::default()
            .hash_password(&password, &SaltString::generate(&mut rand::rngs::OsRng))
            .unwrap()
            .to_string();
        include_str!("../examples/config.example.yaml")
            .replace("<GENERATE_LOCALLY_WITH_HASH_PASSWORD>", &hash)
    }
    fn load(text: &str) -> Result<Config> {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.yaml");
        std::fs::write(&path, text).unwrap();
        Config::from_file(&path)
    }
    #[test]
    fn loads_yaml_and_resolves_database_against_configuration_directory() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.yaml");
        std::fs::write(&path, yaml()).unwrap();
        let c = Config::from_file(&path).unwrap();
        assert_eq!(
            Path::new(&c.database),
            temp.path().canonicalize().unwrap().join("comments.sqlite3")
        );
        assert_eq!(c.proxy_mode, ProxyMode::Direct);
        assert!(c.cookie_secure);
        assert_eq!(c.smtp_port, 587);
        assert!(!Path::new(&c.database).exists());
    }
    #[test]
    fn rejects_invalid_schema_types_and_duplicate_fields_without_disclosing_values() {
        let valid = yaml();
        for bad in [
            format!("{valid}\nunknown_field: sensitive-marker"),
            valid.replace("cookie_secure: true", "cookie_secure: sensitive-marker"),
            format!("{valid}\nbind: sensitive-marker"),
            valid.replace("proxy_mode: direct", "proxy_mode: sensitive-marker"),
            "smtp_password: [sensitive-marker".into(),
        ] {
            let error = load(&bad).err().unwrap().to_string();
            assert!(!error.contains("sensitive-marker"));
        }
        assert!(load("").is_err());
        assert!(Config::from_file(Path::new("does-not-exist.yaml")).is_err());
    }
    #[test]
    fn validates_origins_password_proxy_and_mail_settings() {
        let valid = yaml();
        for bad in [
            valid.replace("https://blog.example", "*"),
            valid.replace("'https://blog.example'", "'https://user@example.com'"),
            valid
                .replace("proxy_mode: direct", "proxy_mode: cloudflare")
                .replace("127.0.0.1:8787", "0.0.0.0:8787"),
            valid.replace("database: 'comments.sqlite3'", "database: ''"),
            valid.replace("smtp_host: ''", "smtp_host: 'smtp.example.com'"),
            include_str!("../examples/config.example.yaml").into(),
        ] {
            assert!(load(&bad).is_err());
        }
        for mode in ["direct", "nginx", "cloudflare"] {
            assert!(
                load(&valid.replace("proxy_mode: direct", &format!("proxy_mode: {mode}"))).is_ok()
            );
        }
    }
}
