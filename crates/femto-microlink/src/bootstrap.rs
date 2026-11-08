//! Bounded coordinator key exchange over an already authenticated stream.
use crate::protocol::key::{Machine, PublicKey};
use std::io::{self, BufRead, BufReader, Read, Write};
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid coordinator bootstrap response",
    )
}

pub fn authority(host: &str, port: u16) -> io::Result<String> {
    if port == 0
        || host.is_empty()
        || host.len() > 253
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid control hostname",
        ));
    }
    Ok(if port == 443 {
        host.into()
    } else {
        format!("{host}:{port}")
    })
}
/// The caller must authenticate the TLS peer against host and enforce an
/// absolute deadline and cancellation across every read/write and result release.
pub fn exchange<S: Read + Write>(
    mut stream: S,
    host: &str,
    port: u16,
    version: u16,
) -> io::Result<PublicKey<Machine>> {
    let authority = authority(host, port)?;
    write!(
        stream,
        "GET /key?v={version} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n"
    )?;
    stream.flush()?;
    parse_key(&read_body(stream)?)
}

fn line<R: BufRead>(reader: &mut R, limit: usize) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_until(b'\n', &mut line)?;
    if line.len() > limit || !line.ends_with(b"\r\n") {
        return Err(invalid());
    }
    Ok(line)
}

fn headers<R: BufRead>(reader: &mut R, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let next = line(reader, limit - bytes.len())?;
        let done = next == b"\r\n";
        bytes.extend_from_slice(&next);
        if done {
            return Ok(bytes);
        }
    }
}

fn read_body<R: Read>(reader: R) -> io::Result<Vec<u8>> {
    let mut reader = BufReader::new(reader.take(16 * 1024));
    let bytes = headers(&mut reader, 8192)?;
    let mut fields = [httparse::EMPTY_HEADER; 64];
    let mut response = httparse::Response::new(&mut fields);
    if !response.parse(&bytes).map_err(|_| invalid())?.is_complete() || response.code != Some(200) {
        return Err(invalid());
    }
    let mut length = None;
    let mut chunked = false;
    let mut encoding = false;
    for h in response.headers.iter() {
        if h.name.eq_ignore_ascii_case("content-length") {
            if length.is_some() || h.value.is_empty() || !h.value.iter().all(u8::is_ascii_digit) {
                return Err(invalid());
            }
            let value = std::str::from_utf8(h.value)
                .map_err(|_| invalid())?
                .parse::<usize>()
                .map_err(|_| invalid())?;
            if value > 4096 {
                return Err(invalid());
            }
            length = Some(value);
        } else if h.name.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !h.value.eq_ignore_ascii_case(b"chunked") {
                return Err(invalid());
            }
            chunked = true;
        } else if h.name.eq_ignore_ascii_case("content-encoding") {
            if encoding || !h.value.eq_ignore_ascii_case(b"identity") {
                return Err(invalid());
            }
            encoding = true;
        }
    }
    if chunked && length.is_some() {
        return Err(invalid());
    }
    let mut body = Vec::new();
    if chunked {
        for _ in 0..256 {
            let chunk_line = line(&mut reader, 256)?;
            let httparse::Status::Complete((_, size)) =
                httparse::parse_chunk_size(&chunk_line).map_err(|_| invalid())?
            else {
                return Err(invalid());
            };
            if size == 0 {
                let trailers = headers(&mut reader, 2048)?;
                let mut fields = [httparse::EMPTY_HEADER; 16];
                if !httparse::parse_headers(&trailers, &mut fields)
                    .map_err(|_| invalid())?
                    .is_complete()
                {
                    return Err(invalid());
                }
                return Ok(body);
            }
            let size = usize::try_from(size).map_err(|_| invalid())?;
            if size > 4096 - body.len() {
                return Err(invalid());
            }
            let start = body.len();
            body.resize(start + size, 0);
            reader.read_exact(&mut body[start..])?;
            let mut end = [0; 2];
            reader.read_exact(&mut end)?;
            if end != *b"\r\n" {
                return Err(invalid());
            }
        }
        return Err(invalid());
    } else if let Some(length) = length {
        body.resize(length, 0);
        reader.read_exact(&mut body)?;
    } else {
        reader.take(4097).read_to_end(&mut body)?;
        if body.len() > 4096 {
            return Err(invalid());
        }
    }
    Ok(body)
}

fn parse_key(body: &[u8]) -> io::Result<PublicKey<Machine>> {
    #[derive(serde::Deserialize)]
    struct Keys {
        #[serde(rename = "publicKey")]
        public_key: String,
    }
    let keys: Keys = serde_json::from_slice(body).map_err(|_| invalid())?;
    let key = PublicKey::parse(&keys.public_key).map_err(|_| invalid())?;
    if key == PublicKey::zero() {
        return Err(invalid());
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fragments<'a>(&'a [u8]);
    impl Read for Fragments<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let n = out.len().min(1).min(self.0.len());
            out[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }
    #[test]
    fn body_framing_and_fragmentation() {
        for wire in [
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\n{\r\n1;foo=bar\r\n}\r\n0\r\nX-Test: yes\r\n\r\n",
            "HTTP/1.0 200 OK\r\n\r\n{}",
        ] {
            assert_eq!(read_body(Fragments(wire.as_bytes())).unwrap(), b"{}");
        }
    }
    #[test]
    fn invalid_http_and_bounds() {
        for wire in [
            "HTTP/1.1 302 Found\r\nLocation: https://elsewhere\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 4097\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1001\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx!!0\r\n\r\n",
        ] {
            assert!(read_body(wire.as_bytes()).is_err(), "{wire}");
        }
        assert!(
            read_body(format!("HTTP/1.1 200 OK\r\nX: {}\r\n\r\n", "a".repeat(8192)).as_bytes())
                .is_err()
        );
        assert!(
            read_body(format!("HTTP/1.1 200 OK\r\n\r\n{}", "a".repeat(4097)).as_bytes()).is_err()
        );
    }
    #[test]
    fn key_schema_and_nonzero_requirement() {
        let key = format!("mkey:{}", "01".repeat(32));
        let body = format!(r#"{{"publicKey":"{key}","legacyPublicKey":"ignored"}}"#);
        assert_eq!(parse_key(body.as_bytes()).unwrap().to_string(), key);
        for body in [
            "{}".to_owned(),
            format!(r#"{{"publicKey":"mkey:{}"}}"#, "00".repeat(32)),
            format!(r#"{{"publicKey":"{key}","publicKey":"{key}"}}"#),
        ] {
            assert!(parse_key(body.as_bytes()).is_err());
        }
    }
}
