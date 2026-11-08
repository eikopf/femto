//! Explicit Basic credentials scoped to one URL origin.
use base64::{Engine, engine::general_purpose::STANDARD};
use url::{Origin, Url};
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct BasicAuth {
    origin: Origin,
    header: Zeroizing<String>,
}
impl BasicAuth {
    /// Device configuration permits plaintext HTTP only through an explicit field.
    /// Errors never contain credential values or serde's input excerpts.
    pub fn from_json(source: &str, bytes: &[u8]) -> Result<Self, crate::Error> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            username: Zeroizing<String>,
            password: Zeroizing<String>,
            #[serde(default)]
            allow_http: bool,
        }
        if bytes.len() > 8192 {
            return Err(crate::Error::Limit("credential file"));
        }
        let input: Input =
            serde_json::from_slice(bytes).map_err(|_| crate::Error::Field("credential JSON"))?;
        Self::new(source, &input.username, &input.password, input.allow_http)
    }
    /// ASCII credentials avoid ambiguous legacy Basic charset handling. HTTP
    /// requires an explicit opt-in; HTTPS is the default credential transport.
    pub fn new(
        source: &str,
        username: &str,
        password: &str,
        allow_http: bool,
    ) -> Result<Self, crate::Error> {
        let url = crate::web_url(None, source)?;
        if url.scheme() != "https" && !allow_http {
            return Err(crate::Error::Field(
                "Basic authentication requires HTTPS or explicit HTTP opt-in",
            ));
        }
        if username.len() > 1024
            || password.len() > 1024
            || username.contains(':')
            || username
                .bytes()
                .chain(password.bytes())
                .any(|b| !b.is_ascii() || b.is_ascii_control())
        {
            return Err(crate::Error::Field("Basic credentials"));
        }
        let plain = Zeroizing::new(format!("{username}:{password}"));
        let mut header = Zeroizing::new(String::from("Basic "));
        STANDARD.encode_string(plain.as_bytes(), &mut header);
        Ok(Self {
            origin: url.origin(),
            header,
        })
    }
    pub(crate) fn header_for(&self, url: &Url) -> Option<&str> {
        (url.origin() == self.origin).then_some(self.header.as_str())
    }
}
impl std::fmt::Debug for BasicAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BasicAuth([redacted])")
    }
}
