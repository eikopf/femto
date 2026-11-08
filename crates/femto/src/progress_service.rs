//! Single-owner local progress service with bounded requests and conditional storage.
use anyhow::{Context, Result};
use femto_catalog::http::StrongEtag;
use femto_reader::{
    progress::{MAX_PROGRESS_BYTES, Progress},
    state::atomic_write,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_RECORDS: usize = 256;
const MAX_STORE_BYTES: u64 = 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Records {
    version: u32,
    records: BTreeMap<String, Progress>,
}
struct Store {
    owner: crate::state_file::StateFile,
}
impl Store {
    fn open(path: &std::path::Path) -> Result<Self> {
        let store = Self {
            owner: crate::state_file::StateFile::acquire(path)?,
        };
        store.load()?;
        Ok(store)
    }
    fn load(&self) -> Result<Records> {
        let file = match File::open(self.owner.path()) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Records {
                    version: 1,
                    records: BTreeMap::new(),
                });
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_STORE_BYTES + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_STORE_BYTES,
            "progress store size limit"
        );
        let records: Records = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            records.version == 1 && records.records.len() <= MAX_RECORDS,
            "invalid progress store version or count"
        );
        for (id, record) in &records.records {
            let validated = Progress::decode(&serde_json::to_vec(record)?)?;
            anyhow::ensure!(id == &validated.book_id, "progress store identity mismatch");
        }
        Ok(records)
    }
    fn get(&self, id: &str) -> Result<Option<Progress>> {
        Ok(self.load()?.records.remove(id))
    }
    fn put(&self, record: Progress, condition: &str, create: bool) -> Result<u16> {
        let mut records = self.load()?;
        let current = records.records.get(&record.book_id);
        let matches = if create {
            current.is_none()
        } else {
            current.map(etag).transpose()?.as_deref() == Some(condition)
        };
        if !matches {
            return Ok(412);
        }
        if current.is_none() && records.records.len() == MAX_RECORDS {
            return Ok(507);
        }
        let status = if current.is_some() { 204 } else { 201 };
        records.records.insert(record.book_id.clone(), record);
        let bytes = serde_json::to_vec(&records)?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_STORE_BYTES,
            "progress store size limit"
        );
        atomic_write(self.owner.path(), &bytes)?;
        Ok(status)
    }
}
fn etag(record: &Progress) -> Result<String> {
    Ok(format!(
        "\"{:x}\"",
        Sha256::digest(serde_json::to_vec(record)?)
    ))
}

#[derive(clap::Args)]
pub struct ServeArgs {
    #[arg(long)]
    store: PathBuf,
    /// Local service endpoint; only loopback addresses are accepted.
    #[arg(long, default_value = "127.0.0.1:8787")]
    bind: SocketAddr,
}
pub fn serve(args: ServeArgs) -> Result<()> {
    anyhow::ensure!(
        args.bind.ip().is_loopback(),
        "progress service requires a loopback bind address"
    );
    let store = Store::open(&args.store)?;
    let listener = TcpListener::bind(args.bind)?;
    listener.set_nonblocking(true)?;
    let mut interrupts = crate::catalog::Interrupts::new()?;
    let operation = interrupts.begin();
    let cancelled = operation.flag();
    println!(
        "{}",
        serde_json::json!({"listening":listener.local_addr()?.to_string()})
    );
    io::stdout().flush()?;
    while !cancelled.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((tcp, _)) => {
                let mut stream = Deadline {
                    tcp,
                    end: Instant::now() + Duration::from_secs(5),
                    cancelled: cancelled.clone(),
                };
                if let Err(error) = handle(&mut stream, &store) {
                    eprintln!(
                        "Progress request failed: {}",
                        serde_json::to_string(&error.to_string())?
                    );
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
struct Deadline {
    tcp: TcpStream,
    end: Instant,
    cancelled: Arc<AtomicBool>,
}
impl Deadline {
    fn timeout(&self) -> io::Result<Duration> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "service cancelled",
            ));
        }
        self.end
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .map(|d| d.min(Duration::from_millis(100)))
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "request deadline"))
    }
}
impl Read for Deadline {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        loop {
            self.tcp.set_read_timeout(Some(self.timeout()?))?;
            match self.tcp.read(b) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
}
impl Write for Deadline {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        loop {
            self.tcp.set_write_timeout(Some(self.timeout()?))?;
            match self.tcp.write(b) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}
fn respond(stream: &mut impl Write, status: u16, body: &[u8], tag: Option<&str>) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} Response\r\nConnection: close\r\n"
    )?;
    if let Some(tag) = tag {
        write!(stream, "ETag: {tag}\r\n")?;
    }
    if status != 204 {
        write!(
            stream,
            "Content-Length: {}\r\nContent-Type: application/json\r\n",
            body.len()
        )?;
    }
    stream.write_all(b"\r\n")?;
    if status != 204 {
        stream.write_all(body)?;
    }
    stream.flush()?;
    Ok(())
}
fn handle(stream: &mut (impl Read + Write), store: &Store) -> Result<()> {
    let mut reader = BufReader::new(stream);
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        if bytes.len() == 16 * 1024 {
            return respond(reader.get_mut(), 431, b"{}", None);
        }
        let mut byte = [0];
        reader.read_exact(&mut byte)?;
        bytes.push(byte[0]);
    }
    let mut fields = [httparse::EMPTY_HEADER; 32];
    let mut request = httparse::Request::new(&mut fields);
    if !matches!(request.parse(&bytes), Ok(httparse::Status::Complete(_)))
        || request.version != Some(1)
    {
        return respond(reader.get_mut(), 400, b"{}", None);
    }
    let mut headers = BTreeMap::new();
    for field in request.headers.iter() {
        let name = field.name.to_ascii_lowercase();
        if headers.insert(name, field.value).is_some() {
            return respond(reader.get_mut(), 400, b"{}", None);
        }
    }
    if !headers.contains_key("host")
        || headers.contains_key("transfer-encoding")
        || headers.contains_key("content-encoding")
        || headers.contains_key("expect")
    {
        return respond(reader.get_mut(), 400, b"{}", None);
    }
    let id = request
        .path
        .unwrap_or("")
        .strip_prefix("/progress/")
        .unwrap_or("");
    if id.len() != 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return respond(reader.get_mut(), 404, b"{}", None);
    }
    if request.method == Some("GET") {
        if headers.get("content-length").is_some_and(|v| *v != b"0") {
            return respond(reader.get_mut(), 400, b"{}", None);
        }
        return match store.get(id)? {
            Some(record) => respond(
                reader.get_mut(),
                200,
                &serde_json::to_vec(&record)?,
                Some(&etag(&record)?),
            ),
            None => respond(reader.get_mut(), 404, b"{}", None),
        };
    }
    if request.method != Some("PUT") {
        return respond(reader.get_mut(), 405, b"{}", None);
    }
    if !headers
        .get("content-type")
        .is_some_and(|v| v.eq_ignore_ascii_case(b"application/json"))
    {
        return respond(reader.get_mut(), 415, b"{}", None);
    }
    let Some(length) = headers
        .get("content-length")
        .filter(|v| !v.is_empty() && v.iter().all(u8::is_ascii_digit))
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|v| v.parse::<usize>().ok())
    else {
        return respond(reader.get_mut(), 411, b"{}", None);
    };
    if length > MAX_PROGRESS_BYTES {
        return respond(reader.get_mut(), 413, b"{}", None);
    }
    let (condition, create) = match (headers.get("if-match"), headers.get("if-none-match")) {
        (Some(tag), None) => match std::str::from_utf8(tag)
            .ok()
            .filter(|tag| StrongEtag::parse(tag).is_ok())
        {
            Some(tag) => (tag, false),
            None => return respond(reader.get_mut(), 400, b"{}", None),
        },
        (None, Some(tag)) if *tag == b"*" => ("*", true),
        (None, None) => return respond(reader.get_mut(), 428, b"{}", None),
        _ => return respond(reader.get_mut(), 400, b"{}", None),
    };
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let record = match Progress::decode(&body) {
        Ok(record) if record.book_id == id => record,
        _ => return respond(reader.get_mut(), 400, b"{}", None),
    };
    // PUT validators describe the exact accepted representation, without transformation.
    if serde_json::to_vec(&record)? != body {
        return respond(reader.get_mut(), 422, b"{}", None);
    }
    let tag = etag(&record)?;
    let status = store
        .put(record, condition, create)
        .context("store update failed; commit may be unknown")?;
    respond(
        reader.get_mut(),
        status,
        b"{}",
        matches!(status, 201 | 204).then_some(tag.as_str()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(n: usize) -> Progress {
        Progress {
            version: 1,
            book_id: format!("{n:064x}"),
            position: Default::default(),
            recorded_at: 1,
        }
    }
    #[test]
    fn store_conditions_survive_restart_and_rejected_writes_preserve_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.json");
        let store = Store::open(&path).unwrap();
        assert!(Store::open(&path).is_err());
        let initial = record(1);
        assert_eq!(store.put(initial.clone(), "*", true).unwrap(), 201);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(store.put(initial.clone(), "*", true).unwrap(), 412);
        assert_eq!(store.put(initial.clone(), "\"stale\"", false).unwrap(), 412);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let mut updated = initial.clone();
        updated.position.byte = 12;
        assert_eq!(
            store
                .put(updated.clone(), &etag(&initial).unwrap(), false)
                .unwrap(),
            204
        );
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.get(&initial.book_id).unwrap(), Some(updated));
        drop(store);
        std::fs::write(&path, b"broken storage").unwrap();
        assert!(Store::open(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"broken storage");
    }
    #[test]
    fn store_capacity_allows_updates_but_no_additional_book() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.json");
        let records = Records {
            version: 1,
            records: (0..MAX_RECORDS)
                .map(|n| {
                    let record = record(n);
                    (record.book_id.clone(), record)
                })
                .collect(),
        };
        atomic_write(&path, &serde_json::to_vec(&records).unwrap()).unwrap();
        let store = Store::open(&path).unwrap();
        assert_eq!(store.put(record(MAX_RECORDS), "*", true).unwrap(), 507);
        assert_eq!(
            store
                .put(record(0), &etag(&record(0)).unwrap(), false)
                .unwrap(),
            204
        );
    }
    struct Wire {
        input: io::Cursor<Vec<u8>>,
        output: Vec<u8>,
    }
    impl Read for Wire {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            let n = b.len().min(3);
            self.input.read(&mut b[..n])
        }
    }
    impl Write for Wire {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    fn request(store: &Store, input: Vec<u8>, status: u16) -> String {
        let mut wire = Wire {
            input: io::Cursor::new(input),
            output: Vec::new(),
        };
        handle(&mut wire, store).unwrap();
        let response = String::from_utf8(wire.output).unwrap();
        assert!(
            response.starts_with(&format!("HTTP/1.1 {status} ")),
            "{response}"
        );
        response
    }
    #[test]
    fn requests_enforce_framing_identity_preconditions_and_exact_representation() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("store.json")).unwrap();
        let record = record(1);
        let body = serde_json::to_vec(&record).unwrap();
        let path = format!("/progress/{}", record.book_id);
        let put = |extra: &str, body: &[u8]| {
            [format!("PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}\r\n", body.len()).into_bytes(), body.to_vec()].concat()
        };
        request(&store, put("", &body), 428);
        request(
            &store,
            put("If-None-Match: *\r\nTransfer-Encoding: chunked\r\n", &body),
            400,
        );
        request(
            &store,
            put("If-None-Match: *\r\nContent-Length: 0\r\n", &body),
            400,
        );
        request(&store, put("If-Match: W/\"x\"\r\n", &body), 400);
        request(
            &store,
            put(
                "If-None-Match: *\r\n",
                &serde_json::to_vec(&self::record(2)).unwrap(),
            ),
            400,
        );
        request(
            &store,
            put(
                "If-None-Match: *\r\n",
                &serde_json::to_vec_pretty(&record).unwrap(),
            ),
            422,
        );
        request(&store, put("If-None-Match: *\r\n", &vec![b' '; 4097]), 413);
        request(&store, put("If-None-Match: *\r\n", &body), 201);
        request(&store, put("If-None-Match: *\r\n", &body), 412);
        let response = request(
            &store,
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").into_bytes(),
            200,
        );
        assert!(response.ends_with(std::str::from_utf8(&body).unwrap()));
        assert!(response.contains(&format!("ETag: {}\r\n", etag(&record).unwrap())));
        request(
            &store,
            b"GET /progress/../store HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
            404,
        );
        request(&store, vec![b'a'; 16 * 1024], 431);
    }
}
