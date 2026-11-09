//! KOReader's v1 sync protocol over the shared LAN/tailnet stream interface.
mod identity;
pub use identity::DocumentId;
#[cfg(feature = "reader")]
pub mod book;
pub mod config;
#[cfg(feature = "reader")]
pub mod session;

use femto_catalog::http::{Transport, Url, read_json_response};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use zeroize::Zeroizing;

pub const DEFAULT_SERVER: &str = "https://sync.koreader.rocks/";
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[cfg(feature = "reader")]
    #[error(transparent)]
    Reader(#[from] femto_reader::Error),
    #[error("invalid KOReader {0}")]
    Invalid(&'static str),
    #[error("KOReader authentication failed")]
    Unauthorized,
    #[error("KOReader server returned HTTP {0}")]
    Status(u16),
    #[error("invalid KOReader server response")]
    Response,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Http(#[from] femto_catalog::http::Error),
}

#[derive(Clone)]
pub struct Credentials {
    username: Zeroizing<String>,
    key: Zeroizing<String>,
}
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials([redacted])")
    }
}
impl Credentials {
    pub fn from_password(username: &str, password: &str) -> Result<Self, Error> {
        if password.is_empty() || password.len() > 4096 {
            return Err(Error::Invalid("password"));
        }
        let key = Zeroizing::new(format!("{:x}", Md5::digest(password.as_bytes())));
        Self::from_key(username, &key)
    }
    pub fn from_key(username: &str, key: &str) -> Result<Self, Error> {
        if username.is_empty()
            || username.len() > 256
            || username.trim() != username
            || username.contains(':')
            || username.chars().any(char::is_control)
            || key.len() != 32
            || !key
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Invalid("credentials"));
        }
        Ok(Self {
            username: Zeroizing::new(username.into()),
            key: Zeroizing::new(key.into()),
        })
    }
}

/// The position is a KOReader XPointer for reflowable books, or a page number
/// for paged documents. Mapping it to a reader location belongs to the adapter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub document: DocumentId,
    pub progress: String,
    pub percentage: f64,
    pub device: String,
    #[serde(default)]
    pub device_id: String,
}
impl Progress {
    pub fn validate(&self) -> Result<(), Error> {
        if !self.percentage.is_finite()
            || !(0.0..=1.0).contains(&self.percentage)
            || !text(&self.progress, 8192)
            || !text(&self.device, 256)
            || self.device_id.len() > 256
            || self.device_id.chars().any(char::is_control)
        {
            return Err(Error::Invalid("progress"));
        }
        Ok(())
    }
}
fn text(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RemoteProgress {
    #[serde(flatten)]
    pub position: Progress,
    pub timestamp: u64,
}
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Receipt {
    pub document: DocumentId,
    pub timestamp: u64,
}

#[derive(Clone)]
pub struct Client {
    base: Url,
    credentials: Credentials,
}
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KOReaderClient([redacted])")
    }
}
impl Client {
    pub fn new(server: &str, credentials: Credentials, allow_http: bool) -> Result<Self, Error> {
        let mut base = Url::parse(server).map_err(|_| Error::Invalid("server URL"))?;
        if server.len() > 4096
            || !matches!(base.scheme(), "https" | "http")
            || (base.scheme() == "http" && !allow_http)
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(Error::Invalid("server URL"));
        }
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        Ok(Self { base, credentials })
    }
    pub fn server(&self) -> &str {
        self.base.as_str()
    }

    pub fn register(
        &self,
        transport: &mut impl Transport,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Registration<'a> {
            username: &'a str,
            password: &'a str,
        }
        let body = Zeroizing::new(
            serde_json::to_vec(&Registration {
                username: &self.credentials.username,
                password: &self.credentials.key,
            })
            .map_err(|_| Error::Invalid("credentials"))?,
        );
        let bytes = self.request(
            transport,
            "POST",
            "users/create",
            &body,
            201,
            deadline,
            cancelled,
        )?;
        #[derive(Deserialize)]
        struct Registered {
            username: String,
        }
        let registered: Registered = serde_json::from_slice(&bytes).map_err(|_| Error::Response)?;
        if registered.username != *self.credentials.username {
            return Err(Error::Response);
        }
        Ok(())
    }
    pub fn authorize(
        &self,
        transport: &mut impl Transport,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(), Error> {
        let bytes = self.request(
            transport,
            "GET",
            "users/auth",
            &[],
            200,
            deadline,
            cancelled,
        )?;
        #[derive(Deserialize)]
        struct Authorized {
            authorized: String,
        }
        let reply: Authorized = serde_json::from_slice(&bytes).map_err(|_| Error::Response)?;
        if reply.authorized != "OK" {
            return Err(Error::Response);
        }
        Ok(())
    }
    pub fn pull(
        &self,
        transport: &mut impl Transport,
        document: &DocumentId,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Option<RemoteProgress>, Error> {
        let bytes = self.request(
            transport,
            "GET",
            &format!("syncs/progress/{}", document.as_str()),
            &[],
            200,
            deadline,
            cancelled,
        )?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| Error::Response)?;
        if value.as_object().is_some_and(|object| object.is_empty()) {
            return Ok(None);
        }
        let remote: RemoteProgress = serde_json::from_value(value).map_err(|_| Error::Response)?;
        remote.position.validate().map_err(|_| Error::Response)?;
        if &remote.position.document != document || remote.timestamp == 0 {
            return Err(Error::Response);
        }
        Ok(Some(remote))
    }
    pub fn push(
        &self,
        transport: &mut impl Transport,
        progress: &Progress,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Receipt, Error> {
        progress.validate()?;
        let body = serde_json::to_vec(progress).map_err(|_| Error::Invalid("progress"))?;
        let bytes = self.request(
            transport,
            "PUT",
            "syncs/progress",
            &body,
            200,
            deadline,
            cancelled,
        )?;
        let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|_| Error::Response)?;
        if receipt.document != progress.document || receipt.timestamp == 0 {
            return Err(Error::Response);
        }
        Ok(receipt)
    }

    #[allow(clippy::too_many_arguments)]
    fn request(
        &self,
        transport: &mut impl Transport,
        method: &str,
        path: &str,
        body: &[u8],
        status: u16,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Vec<u8>, Error> {
        check(deadline, &cancelled)?;
        let url = self
            .base
            .join(path)
            .map_err(|_| Error::Invalid("endpoint"))?;
        let mut stream = transport.connect(&url, deadline, cancelled.clone())?;
        check(deadline, &cancelled)?;
        let authority = &url[url::Position::BeforeHost..url::Position::AfterPort];
        write!(
            stream,
            "{method} {} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/vnd.koreader.v1+json\r\nAccept-Encoding: identity\r\nConnection: close\r\nx-auth-user: {}\r\nx-auth-key: {}\r\n",
            url.path(),
            self.credentials.username.as_str(),
            self.credentials.key.as_str()
        )?;
        if method != "GET" {
            write!(
                stream,
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                body.len()
            )?;
        }
        stream.write_all(b"\r\n")?;
        stream.write_all(body)?;
        stream.flush()?;
        let reply = read_json_response(stream, MAX_RESPONSE_BYTES, deadline, &cancelled);
        check(deadline, &cancelled)?;
        let (actual, bytes) = reply?;
        if actual == 401 {
            return Err(Error::Unauthorized);
        }
        if actual != status {
            return Err(Error::Status(actual));
        }
        Ok(bytes)
    }
}
fn check(deadline: Instant, cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "KOReader request cancelled",
        )
        .into());
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "KOReader request deadline").into());
    }
    Ok(())
}
