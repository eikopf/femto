//! Registration outcomes from an authenticated coordinator response.
use crate::protocol::{
    control::RegisterResponse,
    key::{Node, PublicKey},
};

const RESPONSE_LIMIT: usize = 16 * 1024;

#[derive(Clone, Eq, PartialEq)]
pub struct ApprovalUrl(url::Url);
impl ApprovalUrl {
    /// Validates an approval URL before presentation or a follow-up request.
    pub fn parse(value: &str) -> Result<Self, Error> {
        if value.len() > 2048
            || value
                .bytes()
                .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        {
            return Err(Error::Url);
        }
        let parsed = url::Url::parse(value).map_err(|_| Error::Url)?;
        if parsed.scheme() != "https"
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return Err(Error::Url);
        }
        Ok(Self(parsed))
    }
    /// Token-bearing URL for explicit presentation to the user; never auto-fetch.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
impl std::fmt::Debug for ApprovalUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApprovalUrl([redacted])")
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum Outcome {
    Authorized,
    AwaitingAuthorization(Option<ApprovalUrl>),
    Expired,
    Rejected,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum Error {
    #[error("invalid registration node key")]
    Key,
    #[error("registration HTTP status {0}")]
    Status(u16),
    #[error("registration response exceeds limit")]
    Size,
    #[error("invalid registration response")]
    Response,
    #[error("invalid registration approval URL")]
    Url,
    #[error("Tailnet Lock registration is unsupported")]
    TailnetLock,
}

/// A successful registration is bound to exactly one node key. Every new
/// attempt revokes the previous result, including malformed responses.
#[derive(Default)]
pub struct Enrollment {
    authorized: Option<PublicKey<Node>>,
}
impl Enrollment {
    pub fn clear(&mut self) {
        self.authorized = None;
    }
    pub fn permits(&self, key: PublicKey<Node>) -> bool {
        self.authorized == Some(key)
    }
    pub fn apply(
        &mut self,
        key: PublicKey<Node>,
        status: u16,
        body: &[u8],
    ) -> Result<Outcome, Error> {
        self.clear();
        if key == PublicKey::zero() {
            return Err(Error::Key);
        }
        if status != 200 {
            return Err(Error::Status(status));
        }
        if body.len() > RESPONSE_LIMIT {
            return Err(Error::Size);
        }
        let response: RegisterResponse =
            serde_json::from_slice(body).map_err(|_| Error::Response)?;
        let outcome = if !response.error.is_empty() {
            Outcome::Rejected
        } else if response.node_key_expired {
            Outcome::Expired
        } else if !response.node_key_signature.is_empty() {
            return Err(Error::TailnetLock);
        } else if !response.auth_url.is_empty() {
            Outcome::AwaitingAuthorization(Some(ApprovalUrl::parse(&response.auth_url)?))
        } else if response.machine_authorized {
            self.authorized = Some(key);
            Outcome::Authorized
        } else {
            Outcome::AwaitingAuthorization(None)
        };
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key() -> PublicKey<Node> {
        PublicKey::parse(&format!("nodekey:{}", "01".repeat(32))).unwrap()
    }
    #[test]
    fn precedence_and_key_binding() {
        let mut enrollment = Enrollment::default();
        assert_eq!(
            enrollment.apply(key(), 200, br#"{"MachineAuthorized":true}"#),
            Ok(Outcome::Authorized)
        );
        assert!(enrollment.permits(key()));
        assert!(!enrollment.permits(PublicKey::zero()));
        for (body, expected) in [
            (
                r#"{"MachineAuthorized":true,"Error":"secret","AuthURL":"invalid"}"#,
                Outcome::Rejected,
            ),
            (
                r#"{"MachineAuthorized":true,"NodeKeyExpired":true}"#,
                Outcome::Expired,
            ),
            ("{}", Outcome::AwaitingAuthorization(None)),
        ] {
            assert_eq!(enrollment.apply(key(), 200, body.as_bytes()), Ok(expected));
            assert!(!enrollment.permits(key()));
        }
        let pending = enrollment.apply(key(),200,br#"{"MachineAuthorized":true,"AuthURL":"https://login.example.test/a?token=secret"}"#).unwrap();
        assert!(!enrollment.permits(key()));
        assert!(!format!("{pending:?}").contains("secret"));
        let Outcome::AwaitingAuthorization(Some(url)) = pending else {
            panic!()
        };
        assert_eq!(url.as_str(), "https://login.example.test/a?token=secret");
    }
    #[test]
    fn errors_revoke_and_do_not_expose_response() {
        let mut enrollment = Enrollment::default();
        for (status, body, error) in [
            (403, b"secret".to_vec(), Error::Status(403)),
            (200, vec![b' '; RESPONSE_LIMIT + 1], Error::Size),
            (200, b"secret".to_vec(), Error::Response),
            (
                200,
                br#"{"MachineAuthorized":true,"MachineAuthorized":false}"#.to_vec(),
                Error::Response,
            ),
            (
                200,
                br#"{"NodeKeySignature":"AQ=="}"#.to_vec(),
                Error::TailnetLock,
            ),
        ] {
            enrollment
                .apply(key(), 200, br#"{"MachineAuthorized":true}"#)
                .unwrap();
            let result = enrollment.apply(key(), status, &body);
            assert_eq!(result, Err(error));
            assert!(!enrollment.permits(key()));
            assert!(!format!("{result:?}").contains("secret"));
        }
    }
    #[test]
    fn approval_url_constraints() {
        for value in [
            "http://example.com/a",
            "javascript:foo",
            "https://u:p@example.com/a",
            "https://example.com/#secret",
            "https://",
            " https://example.com",
            "https://exam\nple.com",
        ] {
            assert_eq!(ApprovalUrl::parse(value), Err(Error::Url));
        }
        assert_eq!(
            ApprovalUrl::parse(&format!("https://example.com/{}", "a".repeat(2048))),
            Err(Error::Url)
        );
        assert!(ApprovalUrl::parse("https://custom.example:8443/approve?id=1").is_ok());
    }
}
