//! Bounded device enrollment configuration, independent of its storage source.
use serde::Deserialize;
use zeroize::Zeroizing;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    version: u8,
    pub host: String,
    pub port: u16,
    pub hostname: String,
    #[serde(default)]
    auth_key: Option<Zeroizing<String>>,
}
impl Config {
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > 8192 {
            return Err("Tailnet configuration exceeds 8192 bytes");
        }
        let config: Self =
            serde_json::from_slice(bytes).map_err(|_| "Invalid tailnet configuration")?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1
            || crate::bootstrap::authority(&self.host, self.port).is_err()
            || self.hostname.is_empty()
            || self.hostname.len() > 63
            || self.hostname.starts_with('-')
            || self.hostname.ends_with('-')
            || !self
                .hostname
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || self.auth_key.as_ref().is_some_and(|key| {
                key.is_empty() || key.len() > 4096 || !key.bytes().all(|b| b.is_ascii_graphic())
            })
        {
            return Err("Invalid tailnet configuration");
        }
        Ok(())
    }
    pub fn auth_key(&self) -> Option<&str> {
        self.auth_key.as_ref().map(|value| value.as_str())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_auth_and_bounded_strict_configuration() {
        let base = r#"{"version":1,"host":"controlplane.tailscale.com","port":443,"hostname":"femto-picco"}"#;
        assert!(Config::parse(base.as_bytes()).unwrap().auth_key().is_none());
        let auth = base.replace(
            "\"version\":1",
            "\"auth_key\":\"tskey-auth-example\",\"version\":1",
        );
        assert_eq!(
            Config::parse(auth.as_bytes()).unwrap().auth_key(),
            Some("tskey-auth-example")
        );
        for bad in [
            base.replace("443", "0"),
            base.replace("femto-picco", "-bad"),
            base.replace("femto-picco", "bad name"),
            base.replace("\"version\":1", "\"version\":2"),
            base.replace("\"version\":1", "\"version\":1,\"version\":1"),
            base.replace("\"version\":1", "\"unexpected\":true,\"version\":1"),
            auth.replace("tskey-auth-example", "secret\\nvalue"),
            auth.replace("tskey-auth-example", &"x".repeat(4097)),
        ] {
            assert!(Config::parse(bad.as_bytes()).is_err());
        }
        assert!(Config::parse(&vec![b' '; 8193]).is_err());
    }
}
