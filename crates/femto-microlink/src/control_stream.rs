//! Bounded framing for an uncompressed, authenticated coordinator response body.

/// Matches the policy parser's maximum complete map size.
pub const MAX_MAP_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StreamError<E> {
    #[error("map stream requires a new connection")]
    Closed,
    #[error("empty or oversized coordinator map")]
    Length,
    #[error("coordinator map buffer allocation failed")]
    Allocation,
    #[error("truncated coordinator map")]
    Truncated,
    #[error("coordinator map handler rejected a message")]
    Handler(E),
}

/// Consumes only decrypted HTTP body bytes. TLS/Noise authentication, HTTP/2
/// framing, cancellation deadlines, and JSON/map validation belong to the caller.
/// Compression must not be requested by this decoder's caller.
#[derive(Default)]
pub struct MapStream {
    header: [u8; 4],
    header_len: usize,
    body: Vec<u8>,
    expected: usize,
    closed: bool,
}

impl MapStream {
    /// Invokes the handler synchronously in wire order. It must validate and
    /// apply each map before returning. The slice is borrowed, so retaining a
    /// map requires an explicit copy. No queue of parsed maps is allocated.
    ///
    /// An error permanently closes this stream. The caller must disconnect its
    /// data plane on any error and begin a new full synchronization. Successfully
    /// applied earlier maps are not rolled back if a later map fails.
    pub fn push<E>(
        &mut self,
        mut bytes: &[u8],
        mut handle: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), StreamError<E>> {
        if self.closed {
            return Err(StreamError::Closed);
        }
        while !bytes.is_empty() {
            if self.header_len < 4 {
                let n = bytes.len().min(4 - self.header_len);
                self.header[self.header_len..self.header_len + n].copy_from_slice(&bytes[..n]);
                self.header_len += n;
                bytes = &bytes[n..];
                if self.header_len != 4 {
                    continue;
                }
                self.expected = u32::from_le_bytes(self.header) as usize;
                if !(1..=MAX_MAP_BYTES).contains(&self.expected) {
                    self.close();
                    return Err(StreamError::Length);
                }
                // Reserve once at the protocol bound, avoiding geometric growth
                // past it as fragmented records arrive.
                if self.body.capacity() == 0 && self.body.try_reserve_exact(MAX_MAP_BYTES).is_err()
                {
                    self.close();
                    return Err(StreamError::Allocation);
                }
            }
            let n = bytes.len().min(self.expected - self.body.len());
            self.body.extend_from_slice(&bytes[..n]);
            bytes = &bytes[n..];
            if self.body.len() == self.expected {
                // Also poison on a panicking handler if the caller catches it.
                self.closed = true;
                if let Err(error) = handle(&self.body) {
                    self.close();
                    return Err(StreamError::Handler(error));
                }
                self.closed = false;
                self.body.clear();
                self.header_len = 0;
                self.expected = 0;
            }
        }
        Ok(())
    }

    /// EOF always closes the stream, even at a complete frame boundary.
    pub fn finish<E>(&mut self) -> Result<(), StreamError<E>> {
        if self.closed {
            return Err(StreamError::Closed);
        }
        let truncated = self.header_len != 0;
        self.close();
        if truncated {
            Err(StreamError::Truncated)
        } else {
            Ok(())
        }
    }

    /// Releases the retained frame on cancellation or connection failure.
    pub fn close(&mut self) {
        self.closed = true;
        self.body = Vec::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(body: &[u8]) -> Vec<u8> {
        let mut bytes = (body.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(body);
        bytes
    }

    #[test]
    fn every_split_and_bytewise_delivery_preserve_order() {
        let maps = [
            b"{\"KeepAlive\":true}".as_slice(),
            b"{\"PeersRemoved\":[7]}",
        ];
        let wire: Vec<_> = maps.iter().flat_map(|map| frame(map)).collect();
        for split in 0..=wire.len() {
            let mut stream = MapStream::default();
            let mut seen = Vec::new();
            for chunk in [&wire[..split], &wire[split..]] {
                stream
                    .push(chunk, |body| {
                        seen.push(body.to_vec());
                        Ok::<_, ()>(())
                    })
                    .unwrap();
            }
            assert_eq!(seen, maps);
            stream.finish::<()>().unwrap();
        }
        let mut stream = MapStream::default();
        let mut seen = Vec::new();
        for byte in &wire {
            stream
                .push(&[*byte], |body| {
                    seen.push(body.to_vec());
                    Ok::<_, ()>(())
                })
                .unwrap();
        }
        assert_eq!(seen, maps);
    }

    #[test]
    fn huge_input_chunk_retains_only_one_bounded_map() {
        let body = vec![b' '; MAX_MAP_BYTES];
        let wire = frame(&body).repeat(32);
        let mut stream = MapStream::default();
        let mut count = 0;
        stream
            .push(&wire, |message| {
                assert_eq!(message, body);
                count += 1;
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(count, 32);
        assert_eq!(stream.body.capacity(), MAX_MAP_BYTES);
        assert_eq!(stream.body.len(), 0);
        stream.finish::<()>().unwrap();
        assert_eq!(stream.body.capacity(), 0);
    }

    #[test]
    fn invalid_lengths_and_partial_eof_poison_stream() {
        for length in [0, MAX_MAP_BYTES as u32 + 1, u32::MAX] {
            let mut stream = MapStream::default();
            assert_eq!(
                stream.push(&length.to_le_bytes(), |_| Ok::<_, ()>(())),
                Err(StreamError::Length)
            );
            assert_eq!(stream.body.capacity(), 0);
            assert_eq!(
                stream.push(&frame(b"{}"), |_| Ok::<_, ()>(())),
                Err(StreamError::Closed)
            );
        }
        let wire = frame(b"{}");
        for end in 1..wire.len() {
            let mut stream = MapStream::default();
            stream.push(&wire[..end], |_| Ok::<_, ()>(())).unwrap();
            assert_eq!(stream.finish::<()>(), Err(StreamError::Truncated));
            assert_eq!(stream.body.capacity(), 0);
        }
    }

    #[test]
    fn handler_failure_prevents_later_map_delivery() {
        let wire = frame(b"{}").repeat(3);
        let mut stream = MapStream::default();
        let mut calls = 0;
        let result = stream.push(&wire, |_| {
            calls += 1;
            if calls == 2 {
                Err("invalid map")
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err(StreamError::Handler("invalid map")));
        assert_eq!(calls, 2);
        assert_eq!(stream.body.capacity(), 0);
        assert_eq!(
            stream.push(&wire, |_| Ok::<_, ()>(())),
            Err(StreamError::Closed)
        );
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError<E> {
    #[error("coordinator stream I/O failed: {0}")]
    Io(#[source] std::io::Error),
    #[error("coordinator stream was cancelled")]
    Cancelled,
    #[error(transparent)]
    Decode(#[from] StreamError<E>),
}

/// Drives framing from an authenticated, uncompressed HTTP body reader using a
/// 2 KiB ingress buffer. The reader must impose a finite read deadline; a
/// cancellation flag cannot interrupt a blocking platform read. Callers must
/// invalidate authorization after every return, including orderly EOF.
pub fn read_maps<R: std::io::Read, E>(
    reader: &mut R,
    mut cancelled: impl FnMut() -> bool,
    mut handle: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), ReadError<E>> {
    let mut stream = MapStream::default();
    let mut buffer = [0; 2048];
    loop {
        if cancelled() {
            return Err(ReadError::Cancelled);
        }
        match reader.read(&mut buffer) {
            Ok(0) => return stream.finish().map_err(ReadError::Decode),
            Ok(n) => {
                if cancelled() {
                    return Err(ReadError::Cancelled);
                }
                stream.push(&buffer[..n], &mut handle)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(ReadError::Io(error)),
        }
    }
}

#[cfg(test)]
mod reader_tests {
    use super::*;
    use std::io::{self, Read};

    struct ShortReader {
        bytes: io::Cursor<Vec<u8>>,
        interrupted: bool,
    }
    impl Read for ShortReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::ErrorKind::Interrupted.into());
            }
            self.bytes.read(&mut buf[..1])
        }
    }
    #[test]
    fn reader_handles_interrupts_eof_and_cancellation() {
        let mut reader = ShortReader {
            bytes: io::Cursor::new(vec![2, 0, 0, 0, b'{', b'}']),
            interrupted: false,
        };
        let mut count = 0;
        read_maps(
            &mut reader,
            || false,
            |map| {
                assert_eq!(map, b"{}");
                count += 1;
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        assert_eq!(count, 1);
        let result = read_maps(&mut io::empty(), || true, |_| Ok::<_, ()>(()));
        assert!(matches!(result, Err(ReadError::Cancelled)));
        let mut checks = 0;
        let result = read_maps(
            &mut io::Cursor::new([2, 0, 0, 0, b'{', b'}']),
            || {
                checks += 1;
                checks > 1
            },
            |_| -> Result<(), ()> { panic!("cancelled read delivered a map") },
        );
        assert!(matches!(result, Err(ReadError::Cancelled)));
        let result = read_maps(&mut io::Cursor::new([2, 0]), || false, |_| Ok::<_, ()>(()));
        assert!(matches!(
            result,
            Err(ReadError::Decode(StreamError::Truncated))
        ));
    }
}

#[cfg(all(test, unix, not(target_os = "espidf")))]
mod socket_tests {
    use super::*;
    use std::{
        io::Write,
        net::{TcpListener, TcpStream},
        thread,
        time::Duration,
    };

    #[test]
    fn tcp_body_adapter_delivers_frames_then_reports_midframe_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let writer = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            for part in [vec![2], vec![0, 0, 0, b'{'], vec![b'}', 9, 0, 0, 0, b'{']] {
                socket.write_all(&part).unwrap();
            }
        });
        let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut maps = 0;
        let result = read_maps(
            &mut socket,
            || false,
            |bytes| {
                assert_eq!(bytes, b"{}");
                maps += 1;
                Ok::<_, ()>(())
            },
        );
        writer.join().unwrap();
        assert_eq!(maps, 1);
        assert!(matches!(
            result,
            Err(ReadError::Decode(StreamError::Truncated))
        ));
    }
}
