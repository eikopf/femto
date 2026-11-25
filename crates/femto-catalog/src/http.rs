//! Bounded HTTP/1 catalog requests over caller-supplied LAN or tailnet streams.
use crate::{Catalog, MAX_FEED_BYTES, deadline::Limit};
use std::{
    collections::BTreeSet,
    io::{self, BufRead, BufReader, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
pub use url::Url;

pub trait Stream: Read + Write {}
impl<T: Read + Write> Stream for T {}
/// Implementations enforce the limit on each wait and cancellation, and must document
/// any uncancellable platform operations such as the host DNS resolver.
pub trait Transport {
    fn connect(
        &mut self,
        url: &Url,
        wait: Limit,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn Stream>>;
}
impl<T: Transport + ?Sized> Transport for Box<T> {
    fn connect(
        &mut self,
        url: &Url,
        wait: Limit,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn Stream>> {
        (**self).connect(url, wait, cancelled)
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
fn check(wait: Limit, cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "catalog request cancelled",
        ));
    }
    if wait.expired() {
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
        {
            let slot = if field.name.eq_ignore_ascii_case("location") {
                &mut result.location
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
    wait: Limit,
    cancelled: &AtomicBool,
) -> io::Result<usize> {
    fn copy_exact(
        reader: &mut impl Read,
        output: &mut impl Write,
        mut size: usize,
        wait: Limit,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        let mut buffer = [0; 8192];
        while size > 0 {
            check(wait, cancelled)?;
            let count = size.min(buffer.len());
            reader.read_exact(&mut buffer[..count])?;
            check(wait, cancelled)?;
            output.write_all(&buffer[..count])?;
            size -= count;
        }
        Ok(())
    }
    let mut total = 0;
    if head.chunked {
        // Chunks of data; the empty chunk that ends the body is not one.
        let mut chunks = 0;
        loop {
            check(wait, cancelled)?;
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
            chunks += 1;
            if chunks > 16_384 || size > limit - total {
                return Err(invalid());
            }
            copy_exact(reader, output, size, wait, cancelled)?;
            total += size;
            let mut end = [0; 2];
            reader.read_exact(&mut end)?;
            if end != *b"\r\n" {
                return Err(invalid());
            }
        }
    } else if let Some(length) = head.length {
        copy_exact(reader, output, length, wait, cancelled)?;
        Ok(length)
    } else {
        let mut buffer = [0; 8192];
        loop {
            check(wait, cancelled)?;
            let size = (limit - total + 1).min(buffer.len());
            let count = reader.read(&mut buffer[..size])?;
            if count == 0 {
                return Ok(total);
            }
            if count > limit - total {
                return Err(invalid());
            }
            check(wait, cancelled)?;
            output.write_all(&buffer[..count])?;
            total += count;
        }
    }
}

pub const MAX_EPUB_BYTES: usize = 64 * 1024 * 1024;

/// Reads one bounded JSON response without following redirects. Non-success
/// responses return their status without retaining an untrusted error body.
pub fn read_json_response(
    stream: impl Read,
    limit: usize,
    wait: Limit,
    cancelled: &AtomicBool,
) -> Result<(u16, Vec<u8>), Error> {
    if limit > MAX_FEED_BYTES {
        return Err(invalid().into());
    }
    check(wait, cancelled)?;
    let mut reader = BufReader::new(stream);
    let head = head(&mut reader, limit)?;
    check(wait, cancelled)?;
    if !(200..300).contains(&head.status) {
        return Ok((head.status, Vec::new()));
    }
    let media = head
        .content_type
        .as_deref()
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    let media = media.to_ascii_lowercase();
    if media != "application/json"
        && !media
            .strip_prefix("application/")
            .is_some_and(|subtype| subtype.ends_with("+json"))
    {
        return Err(Error::ContentType);
    }
    let mut bytes = Vec::new();
    body(&mut reader, &head, limit, &mut bytes, wait, cancelled)?;
    check(wait, cancelled)?;
    Ok((head.status, bytes))
}
#[derive(Debug)]
pub struct Download {
    pub source: String,
    pub bytes: usize,
}
#[derive(Clone, Copy)]
enum Kind {
    Feed,
    Description,
    Epub,
    /// Any content up to the caller's limit, from an offset the caller
    /// already holds.
    File {
        limit: usize,
        offset: u64,
    },
}
impl Kind {
    fn limit(self) -> usize {
        match self {
            Self::Feed => MAX_FEED_BYTES,
            Self::Description => 64 * 1024,
            Self::Epub => MAX_EPUB_BYTES,
            Self::File { limit, .. } => limit,
        }
    }
    /// Bytes of the resource to skip; the rest is requested as a range.
    fn offset(self) -> u64 {
        match self {
            Self::File { offset, .. } => offset,
            _ => 0,
        }
    }
    fn accept(self) -> &'static str {
        match self {
            Self::Feed => "application/opds+json, application/json, application/atom+xml",
            Self::Description => "application/opensearchdescription+xml, application/xml, text/xml",
            Self::Epub => "application/epub+zip, application/octet-stream",
            Self::File { .. } => "*/*",
        }
    }
    fn accepts(self, media: &str) -> bool {
        matches!(self, Self::File { .. })
            || self
                .accept()
                .split(", ")
                .any(|allowed| media.eq_ignore_ascii_case(allowed))
    }
}

/// Streams a file of any type into caller-owned storage, following
/// redirects. With an `offset`, only the bytes from there on are requested
/// and the server must honour the range. At most `limit` bytes are
/// received. The caller validates the content.
pub fn download_file(
    transport: &mut impl Transport,
    source: &str,
    wait: Limit,
    cancelled: Arc<AtomicBool>,
    limit: usize,
    offset: u64,
    output: &mut impl Write,
) -> Result<Download, Error> {
    receive(
        transport,
        source,
        wait,
        cancelled,
        None,
        Kind::File { limit, offset },
        output,
    )
}

/// Streams into caller-owned temporary storage. The caller must discard partial
/// output on error and validate the book before publishing it.
pub fn download_epub(
    transport: &mut impl Transport,
    source: &str,
    wait: Limit,
    cancelled: Arc<AtomicBool>,
    auth: Option<&crate::auth::BasicAuth>,
    output: &mut impl Write,
) -> Result<Download, Error> {
    receive(transport, source, wait, cancelled, auth, Kind::Epub, output)
}

pub fn fetch(
    transport: &mut impl Transport,
    source: &str,
    wait: Limit,
    cancelled: Arc<AtomicBool>,
) -> Result<Catalog, Error> {
    fetch_authenticated(transport, source, wait, cancelled, None)
}

/// Loads search results, first reading the OpenSearch description if one is needed.
pub fn fetch_search(
    transport: &mut impl Transport,
    search: &crate::search::Search,
    wait: Limit,
    cancelled: Arc<AtomicBool>,
    auth: Option<&crate::auth::BasicAuth>,
) -> Result<Catalog, Error> {
    use crate::search::Search;
    let source = match search {
        Search::Feed(source) => source.clone(),
        Search::Described { description, query } => {
            let mut bytes = Vec::new();
            let found = receive(
                transport,
                description,
                wait,
                cancelled.clone(),
                auth,
                Kind::Description,
                &mut bytes,
            )?;
            crate::search::described_url(&bytes, &found.source, query)?
        }
    };
    fetch_authenticated(transport, &source, wait, cancelled, auth)
}

pub fn fetch_authenticated(
    transport: &mut impl Transport,
    source: &str,
    wait: Limit,
    cancelled: Arc<AtomicBool>,
    auth: Option<&crate::auth::BasicAuth>,
) -> Result<Catalog, Error> {
    let mut bytes = Vec::new();
    let result = receive(
        transport,
        source,
        wait,
        cancelled,
        auth,
        Kind::Feed,
        &mut bytes,
    )?;
    Ok(Catalog::parse(&bytes, &result.source)?)
}

fn receive(
    transport: &mut impl Transport,
    source: &str,
    wait: Limit,
    cancelled: Arc<AtomicBool>,
    auth: Option<&crate::auth::BasicAuth>,
    kind: Kind,
    output: &mut impl Write,
) -> Result<Download, Error> {
    let offset = kind.offset();
    let mut url = crate::web_url(None, source)?;
    url.set_fragment(None);
    let mut visited = BTreeSet::new();
    for redirect in 0..=5 {
        check(wait, &cancelled)?;
        if !visited.insert(url.to_string()) {
            return Err(Error::Redirect);
        }
        let mut stream = transport.connect(&url, wait, cancelled.clone())?;
        check(wait, &cancelled)?;
        let target = &url[url::Position::BeforePath..url::Position::AfterQuery];
        let authority = &url[url::Position::BeforeHost..url::Position::AfterPort];
        let accept = kind.accept();
        write!(
            stream,
            "GET {target} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: femto\r\nAccept: {accept}\r\nAccept-Encoding: identity\r\nConnection: close\r\n"
        )?;
        if let Some(header) = auth.and_then(|auth| auth.header_for(&url)) {
            write!(stream, "Authorization: {header}\r\n")?;
        }
        if offset > 0 {
            write!(stream, "Range: bytes={offset}-\r\n")?;
        }
        stream.write_all(b"\r\n")?;
        stream.flush()?;
        let mut reader = BufReader::new(stream);
        let head = head(&mut reader, kind.limit())?;
        if matches!(head.status, 301 | 302 | 303 | 307 | 308) {
            let location = head.location.as_deref().ok_or(Error::Redirect)?;
            let next = crate::web_url(Some(&url), location)?;
            if redirect == 5 || (url.scheme() == "https" && next.scheme() == "http") {
                return Err(Error::Redirect);
            }
            url = next;
            url.set_fragment(None);
            continue;
        }
        // A server that ignores the range answers 200 with the whole
        // resource, which must not be appended to the part already held.
        if head.status != if offset > 0 { 206 } else { 200 } {
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
        let bytes = body(&mut reader, &head, kind.limit(), output, wait, &cancelled)?;
        check(wait, &cancelled)?;
        return Ok(Download {
            source: url.to_string(),
            bytes,
        });
    }
    Err(Error::Redirect)
}
