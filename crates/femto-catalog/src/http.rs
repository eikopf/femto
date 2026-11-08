//! Bounded HTTP/1 catalog requests over caller-supplied LAN or tailnet streams.
use crate::{Catalog, MAX_FEED_BYTES};
use std::{
    collections::BTreeSet,
    io::{self, BufRead, BufReader, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
pub use url::Url;

pub trait Stream: Read + Write {}
impl<T: Read + Write> Stream for T {}
/// Implementations enforce socket deadlines and cancellation, and must document
/// any uncancellable platform operations such as the host DNS resolver.
pub trait Transport {
    fn connect(
        &mut self,
        url: &Url,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn Stream>>;
}
impl<T: Transport + ?Sized> Transport for Box<T> {
    fn connect(
        &mut self,
        url: &Url,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn Stream>> {
        (**self).connect(url, deadline, cancelled)
    }
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Catalog(#[from] crate::Error),
    #[error("catalog HTTP status {0}")]
    Status(u16),
    #[error("catalog redirect limit, loop, or HTTPS downgrade")]
    Redirect,
    #[error("unsupported response content type")]
    ContentType,
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid or oversized catalog HTTP response",
    )
}
fn check(deadline: Instant, cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "catalog request cancelled",
        ));
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "catalog request deadline",
        ));
    }
    Ok(())
}
fn line(reader: &mut impl BufRead, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    if bytes.len() > limit || !bytes.ends_with(b"\r\n") {
        return Err(invalid());
    }
    Ok(bytes)
}
fn headers(reader: &mut impl BufRead, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let part = line(reader, limit - bytes.len())?;
        let done = part.len() == 2;
        bytes.extend(part);
        if done {
            return Ok(bytes);
        }
    }
}
struct Head {
    status: u16,
    location: Option<String>,
    content_type: Option<String>,
    etag: Option<String>,
    length: Option<usize>,
    chunked: bool,
}
fn head(reader: &mut impl BufRead, limit: usize) -> io::Result<Head> {
    let bytes = headers(reader, 16 * 1024)?;
    let mut fields = [httparse::EMPTY_HEADER; 64];
    let mut response = httparse::Response::new(&mut fields);
    if !response.parse(&bytes).map_err(|_| invalid())?.is_complete() {
        return Err(invalid());
    }
    let mut result = Head {
        status: response.code.ok_or_else(invalid)?,
        location: None,
        content_type: None,
        etag: None,
        length: None,
        chunked: false,
    };
    let mut encoding = false;
    for field in response.headers.iter() {
        if field.name.eq_ignore_ascii_case("content-length") {
            if result.length.is_some()
                || field.value.is_empty()
                || !field.value.iter().all(u8::is_ascii_digit)
            {
                return Err(invalid());
            }
            let length = std::str::from_utf8(field.value)
                .map_err(|_| invalid())?
                .parse()
                .map_err(|_| invalid())?;
            if length > limit {
                return Err(invalid());
            }
            result.length = Some(length);
        } else if field.name.eq_ignore_ascii_case("transfer-encoding") {
            if result.chunked || !field.value.eq_ignore_ascii_case(b"chunked") {
                return Err(invalid());
            }
            result.chunked = true;
        } else if field.name.eq_ignore_ascii_case("content-encoding") {
            if encoding || !field.value.eq_ignore_ascii_case(b"identity") {
                return Err(invalid());
            }
            encoding = true;
        } else if field.name.eq_ignore_ascii_case("location")
            || field.name.eq_ignore_ascii_case("content-type")
            || field.name.eq_ignore_ascii_case("etag")
        {
            let slot = if field.name.eq_ignore_ascii_case("location") {
                &mut result.location
            } else if field.name.eq_ignore_ascii_case("etag") {
                &mut result.etag
            } else {
                &mut result.content_type
            };
            if slot.is_some() || field.value.len() > 4096 {
                return Err(invalid());
            }
            *slot = Some(
                std::str::from_utf8(field.value)
                    .map_err(|_| invalid())?
                    .to_owned(),
            );
        }
    }
    if result.chunked && result.length.is_some() {
        return Err(invalid());
    }
    Ok(result)
}
fn body(
    reader: &mut impl BufRead,
    head: &Head,
    limit: usize,
    output: &mut impl Write,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> io::Result<usize> {
    fn copy_exact(
        reader: &mut impl Read,
        output: &mut impl Write,
        mut size: usize,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        let mut buffer = [0; 8192];
        while size > 0 {
            check(deadline, cancelled)?;
            let count = size.min(buffer.len());
            reader.read_exact(&mut buffer[..count])?;
            check(deadline, cancelled)?;
            output.write_all(&buffer[..count])?;
            size -= count;
        }
        Ok(())
    }
    let mut total = 0;
    if head.chunked {
        for _ in 0..16_384 {
            check(deadline, cancelled)?;
            let chunk = line(reader, 256)?;
            let httparse::Status::Complete((_, size)) =
                httparse::parse_chunk_size(&chunk).map_err(|_| invalid())?
            else {
                return Err(invalid());
            };
            if size == 0 {
                let trailers = headers(reader, 2048)?;
                let mut fields = [httparse::EMPTY_HEADER; 16];
                if !httparse::parse_headers(&trailers, &mut fields)
                    .map_err(|_| invalid())?
                    .is_complete()
                {
                    return Err(invalid());
                }
                return Ok(total);
            }
            let size = usize::try_from(size).map_err(|_| invalid())?;
            if size > limit - total {
                return Err(invalid());
            }
            copy_exact(reader, output, size, deadline, cancelled)?;
            total += size;
            let mut end = [0; 2];
            reader.read_exact(&mut end)?;
            if end != *b"\r\n" {
                return Err(invalid());
            }
        }
        Err(invalid())
    } else if let Some(length) = head.length {
        copy_exact(reader, output, length, deadline, cancelled)?;
        Ok(length)
    } else {
        let mut buffer = [0; 8192];
        loop {
            check(deadline, cancelled)?;
            let size = (limit - total + 1).min(buffer.len());
            let count = reader.read(&mut buffer[..size])?;
            if count == 0 {
                return Ok(total);
            }
            if count > limit - total {
                return Err(invalid());
            }
            check(deadline, cancelled)?;
            output.write_all(&buffer[..count])?;
            total += count;
        }
    }
}

pub const MAX_EPUB_BYTES: usize = 64 * 1024 * 1024;
#[derive(Debug)]
pub struct Download {
    pub source: String,
    pub bytes: usize,
    pub etag: Option<StrongEtag>,
}
#[derive(Clone, Copy)]
enum Kind {
    ProgressExact,
    Progress,
    Feed,
    Epub,
}
impl Kind {
    fn limit(self) -> usize {
        match self {
            Self::Progress | Self::ProgressExact => 4096,
            Self::Feed => MAX_FEED_BYTES,
            Self::Epub => MAX_EPUB_BYTES,
        }
    }
    fn accept(self) -> &'static str {
        match self {
            Self::Progress | Self::ProgressExact => "application/json",
            Self::Feed => "application/opds+json, application/json",
            Self::Epub => "application/epub+zip, application/octet-stream",
        }
    }
    fn accepts(self, media: &str) -> bool {
        self.accept()
            .split(", ")
            .any(|allowed| media.eq_ignore_ascii_case(allowed))
    }
}

/// Streams into caller-owned temporary storage. The caller must discard partial
/// output on error and validate the book before publishing it.
pub fn download_epub(
    transport: &mut impl Transport,
    source: &str,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    auth: Option<&crate::auth::BasicAuth>,
    output: &mut impl Write,
) -> Result<Download, Error> {
    receive(
        transport,
        source,
        deadline,
        cancelled,
        auth,
        Kind::Epub,
        output,
    )
}

pub fn fetch(
    transport: &mut impl Transport,
    source: &str,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
) -> Result<Catalog, Error> {
    fetch_authenticated(transport, source, deadline, cancelled, None)
}

pub fn fetch_authenticated(
    transport: &mut impl Transport,
    source: &str,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    auth: Option<&crate::auth::BasicAuth>,
) -> Result<Catalog, Error> {
    let mut bytes = Vec::new();
    let result = receive(
        transport,
        source,
        deadline,
        cancelled,
        auth,
        Kind::Feed,
        &mut bytes,
    )?;
    Ok(Catalog::parse(&bytes, &result.source)?)
}

/// Retrieves a Femto progress JSON record, capped at 4096 bytes before decoding.
/// The caller validates book identity and position before changing local state.
pub fn fetch_progress(
    transport: &mut impl Transport,
    source: &str,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    auth: Option<&crate::auth::BasicAuth>,
) -> Result<Vec<u8>, Error> {
    Ok(fetch_progress_versioned(transport, source, deadline, cancelled, auth)?.body)
}

pub struct ProgressResponse {
    pub body: Vec<u8>,
    pub source: String,
    pub etag: Option<StrongEtag>,
}
pub fn fetch_progress_versioned(
    transport: &mut impl Transport,
    source: &str,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    auth: Option<&crate::auth::BasicAuth>,
) -> Result<ProgressResponse, Error> {
    let mut bytes = Vec::new();
    let result = receive(
        transport,
        source,
        deadline,
        cancelled,
        auth,
        Kind::Progress,
        &mut bytes,
    )?;
    Ok(ProgressResponse {
        body: bytes,
        source: result.source,
        etag: result.etag,
    })
}

/// Fetches only the configured endpoint, rejecting redirects even when their target is absent.
pub fn fetch_progress_exact(
    transport: &mut impl Transport,
    source: &str,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    auth: Option<&crate::auth::BasicAuth>,
) -> Result<ProgressResponse, Error> {
    let mut bytes = Vec::new();
    let result = receive(
        transport,
        source,
        deadline,
        cancelled,
        auth,
        Kind::ProgressExact,
        &mut bytes,
    )?;
    Ok(ProgressResponse {
        body: bytes,
        source: result.source,
        etag: result.etag,
    })
}

/// A bounded ASCII strong entity tag. Weak tags and lists cannot authorize replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrongEtag(String);
impl StrongEtag {
    pub fn parse(value: &str) -> io::Result<Self> {
        if !(2..=256).contains(&value.len())
            || !value.starts_with('"')
            || !value.ends_with('"')
            || !value.as_bytes()[1..value.len() - 1]
                .iter()
                .all(|b| *b == 0x21 || (0x23..=0x7e).contains(b))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid strong ETag",
            ));
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
pub enum Precondition<'a> {
    Create,
    Match(&'a StrongEtag),
}

/// Sends one conditional PUT without redirects or retries. An error after sending
/// can mean the server committed the write but its acknowledgement was lost.
pub fn put_progress(
    transport: &mut impl Transport,
    source: &str,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    auth: Option<&crate::auth::BasicAuth>,
    bytes: &[u8],
    precondition: Precondition<'_>,
) -> Result<StrongEtag, Error> {
    if bytes.len() > Kind::Progress.limit() {
        return Err(invalid().into());
    }
    let url = crate::web_url(None, source)?;
    if url.fragment().is_some() {
        return Err(invalid().into());
    }
    check(deadline, &cancelled)?;
    let mut stream = transport.connect(&url, deadline, cancelled.clone())?;
    check(deadline, &cancelled)?;
    let target = &url[url::Position::BeforePath..url::Position::AfterQuery];
    let authority = &url[url::Position::BeforeHost..url::Position::AfterPort];
    write!(
        stream,
        "PUT {target} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccept-Encoding: identity\r\nConnection: close\r\n",
        bytes.len()
    )?;
    match precondition {
        Precondition::Create => stream.write_all(b"If-None-Match: *\r\n")?,
        Precondition::Match(tag) => write!(stream, "If-Match: {}\r\n", tag.as_str())?,
    }
    if let Some(header) = auth.and_then(|auth| auth.header_for(&url)) {
        write!(stream, "Authorization: {header}\r\n")?;
    }
    stream.write_all(b"\r\n")?;
    check(deadline, &cancelled)?;
    stream.write_all(bytes)?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let head = head(&mut reader, Kind::Progress.limit())?;
    if !matches!(head.status, 200 | 201 | 204) {
        return Err(Error::Status(head.status));
    }
    let etag = head
        .etag
        .as_deref()
        .ok_or_else(invalid)
        .and_then(StrongEtag::parse)?;
    if head.status == 204 {
        if head.length.is_some() || head.chunked {
            return Err(invalid().into());
        }
    } else {
        body(
            &mut reader,
            &head,
            Kind::Progress.limit(),
            &mut io::sink(),
            deadline,
            &cancelled,
        )?;
    }
    check(deadline, &cancelled)?;
    Ok(etag)
}

fn receive(
    transport: &mut impl Transport,
    source: &str,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    auth: Option<&crate::auth::BasicAuth>,
    kind: Kind,
    output: &mut impl Write,
) -> Result<Download, Error> {
    let mut url = crate::web_url(None, source)?;
    url.set_fragment(None);
    let mut visited = BTreeSet::new();
    for redirect in 0..=5 {
        check(deadline, &cancelled)?;
        if !visited.insert(url.to_string()) {
            return Err(Error::Redirect);
        }
        let mut stream = transport.connect(&url, deadline, cancelled.clone())?;
        check(deadline, &cancelled)?;
        let target = &url[url::Position::BeforePath..url::Position::AfterQuery];
        let authority = &url[url::Position::BeforeHost..url::Position::AfterPort];
        let accept = kind.accept();
        write!(
            stream,
            "GET {target} HTTP/1.1\r\nHost: {authority}\r\nAccept: {accept}\r\nAccept-Encoding: identity\r\nConnection: close\r\n"
        )?;
        if let Some(header) = auth.and_then(|auth| auth.header_for(&url)) {
            write!(stream, "Authorization: {header}\r\n")?;
        }
        stream.write_all(b"\r\n")?;
        stream.flush()?;
        let mut reader = BufReader::new(stream);
        let head = head(&mut reader, kind.limit())?;
        if matches!(head.status, 301 | 302 | 303 | 307 | 308) {
            if matches!(kind, Kind::ProgressExact) {
                return Err(Error::Redirect);
            }
            let location = head.location.as_deref().ok_or(Error::Redirect)?;
            let next = crate::web_url(Some(&url), location)?;
            if redirect == 5 || (url.scheme() == "https" && next.scheme() == "http") {
                return Err(Error::Redirect);
            }
            url = next;
            url.set_fragment(None);
            continue;
        }
        if head.status != 200 {
            return Err(Error::Status(head.status));
        }
        let media = head
            .content_type
            .as_deref()
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        if !kind.accepts(media) {
            return Err(Error::ContentType);
        }
        let bytes = body(
            &mut reader,
            &head,
            kind.limit(),
            output,
            deadline,
            &cancelled,
        )?;
        check(deadline, &cancelled)?;
        return Ok(Download {
            source: url.to_string(),
            bytes,
            etag: head
                .etag
                .as_deref()
                .and_then(|tag| StrongEtag::parse(tag).ok()),
        });
    }
    Err(Error::Redirect)
}
