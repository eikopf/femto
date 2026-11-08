//! Bounded DERP session over an authenticated transport.
use crate::protocol::{
    derp::{DerpClient, Event, PROTOCOL_VERSION},
    key::{Node, PrivateKey, PublicKey},
};
use std::{
    io::{self, Read, Write},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const MAX_DATAGRAM: usize = 1536;
const OPERATION_BYTES: usize = 32 * 1024;
const MAX_HEADERS: usize = 8192;

/// Implementations must enforce this absolute deadline in blocking I/O.
pub trait DeadlineIo: Read + Write {
    fn set_deadline(&mut self, deadline: Instant);
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Protocol(#[from] crate::protocol::derp::DerpError),
    #[error("invalid DERP session state or server protocol version")]
    State,
    #[error("DERP datagram exceeds the configured bound")]
    Size,
}
struct Budget {
    deadline: Instant,
    bytes: usize,
}
struct Guard<S> {
    io: S,
    budget: Arc<Mutex<Budget>>,
    http: Option<Vec<u8>>,
    header: [u8; 5],
    header_len: usize,
    remaining: usize,
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
impl<S: DeadlineIo> Guard<S> {
    fn prepare(&mut self, requested: usize) -> io::Result<usize> {
        let budget = self
            .budget
            .lock()
            .map_err(|_| invalid("poisoned DERP budget"))?;
        if Instant::now() >= budget.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "DERP operation deadline",
            ));
        }
        if budget.bytes == 0 {
            return Err(invalid("DERP operation byte budget"));
        }
        self.io.set_deadline(budget.deadline);
        let count = requested.min(budget.bytes);
        Ok(count)
    }
    fn charge(&self, count: usize) -> io::Result<()> {
        let mut budget = self
            .budget
            .lock()
            .map_err(|_| invalid("poisoned DERP budget"))?;
        budget.bytes = budget
            .bytes
            .checked_sub(count)
            .ok_or_else(|| invalid("DERP operation byte budget"))?;
        Ok(())
    }
    fn inspect(&mut self, bytes: &[u8]) -> io::Result<()> {
        for byte in bytes {
            if let Some(http) = &mut self.http {
                if http.len() >= MAX_HEADERS {
                    return Err(invalid("DERP HTTP header limit"));
                }
                http.push(*byte);
                if http.ends_with(b"\r\n\r\n") {
                    validate_upgrade(http)?;
                    self.http = None;
                }
            } else if self.remaining != 0 {
                self.remaining -= 1;
            } else {
                self.header[self.header_len] = *byte;
                self.header_len += 1;
                if self.header_len == 5 {
                    let length = u32::from_be_bytes(self.header[1..].try_into().unwrap()) as usize;
                    let valid = match self.header[0] {
                        1 => (40..=256).contains(&length),
                        3 => (40..=4136).contains(&length),
                        5 => (32..=32 + MAX_DATAGRAM).contains(&length),
                        6 => length == 0,
                        8 => (32..=33).contains(&length),
                        0x12 | 0x13 | 0x15 => length == 8,
                        0x14 => length <= 1024,
                        _ => length <= 256,
                    };
                    if !valid {
                        return Err(invalid("invalid or oversized DERP frame"));
                    }
                    self.header_len = 0;
                    self.remaining = length;
                }
            }
        }
        Ok(())
    }
}
fn validate_upgrade(bytes: &[u8]) -> io::Result<()> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| invalid("invalid DERP HTTP header encoding"))?;
    let mut lines = text.split("\r\n");
    let mut status = lines.next().unwrap_or("").split_ascii_whitespace();
    if status.next() != Some("HTTP/1.1") || status.next() != Some("101") {
        return Err(invalid("DERP upgrade rejected"));
    }
    let mut upgrade = false;
    let mut connection = false;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| invalid("invalid DERP header"))?;
        if name.eq_ignore_ascii_case("upgrade") {
            upgrade = value.trim().eq_ignore_ascii_case("derp");
        }
        if name.eq_ignore_ascii_case("connection") {
            connection = value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"));
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(invalid("unsupported DERP upgrade framing"));
        }
    }
    if !upgrade || !connection {
        return Err(invalid("missing DERP upgrade headers"));
    }
    Ok(())
}
impl<S: DeadlineIo> Read for Guard<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        // Do not let the upstream BufReader read beyond a protocol boundary.
        // This makes a later into_inner handoff lossless at completed frames.
        let boundary = if self.http.is_some() {
            1
        } else if self.remaining != 0 {
            self.remaining
        } else {
            5 - self.header_len
        };
        let limit = self.prepare(buf.len().min(1024).min(boundary))?;
        let count = self.io.read(&mut buf[..limit])?;
        self.charge(count)?;
        self.inspect(&buf[..count])?;
        Ok(count)
    }
}
impl<S: DeadlineIo> Write for Guard<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let limit = self.prepare(buf.len())?;
        let count = self.io.write(&buf[..limit])?;
        self.charge(count)?;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.prepare(0)?;
        self.io.flush()
    }
}

pub struct RelaySession<S: DeadlineIo> {
    local_node: PublicKey<Node>,
    client: Option<DerpClient<Guard<S>>>,
    budget: Arc<Mutex<Budget>>,
    timeout: Duration,
}
impl<S: DeadlineIo> RelaySession<S> {
    /// The caller must authenticate the transport before passing it here.
    pub fn connect(
        io: S,
        authority: &str,
        key: PrivateKey<Node>,
        timeout: Duration,
    ) -> Result<Self, Error> {
        if timeout.is_zero() || timeout > Duration::from_secs(60) {
            return Err(Error::State);
        }
        let budget = Arc::new(Mutex::new(Budget {
            deadline: Instant::now() + timeout,
            bytes: OPERATION_BYTES,
        }));
        let guard = Guard {
            io,
            budget: budget.clone(),
            http: Some(Vec::new()),
            header: [0; 5],
            header_len: 0,
            remaining: 0,
        };
        let local_node = key.public();
        let mut client = DerpClient::connect(guard, authority, key)?;
        match client.receive()? {
            Event::ServerInfo(info) if info.version == PROTOCOL_VERSION => (),
            _ => return Err(Error::State),
        }
        Ok(Self {
            local_node,
            client: Some(client),
            budget,
            timeout,
        })
    }
    fn operation<T>(
        &mut self,
        f: impl FnOnce(&mut DerpClient<Guard<S>>) -> Result<T, crate::protocol::derp::DerpError>,
    ) -> Result<T, Error> {
        let client = self.client.as_mut().ok_or(Error::State)?;
        *self.budget.lock().map_err(|_| Error::State)? = Budget {
            deadline: Instant::now() + self.timeout,
            bytes: OPERATION_BYTES,
        };
        match f(client) {
            Ok(value) => Ok(value),
            Err(error) => {
                self.client = None;
                Err(error.into())
            }
        }
    }
    pub fn send(&mut self, peer: PublicKey<Node>, packet: &[u8]) -> Result<(), Error> {
        if packet.len() > MAX_DATAGRAM {
            return Err(Error::Size);
        }
        self.operation(|client| client.send(peer, packet))
    }
    pub fn receive(&mut self) -> Result<Event, Error> {
        self.operation(DerpClient::receive)
    }
    pub fn note_preferred(&mut self, preferred: bool) -> Result<(), Error> {
        self.operation(|client| client.note_preferred(preferred))
    }
    pub fn local_node(&self) -> PublicKey<Node> {
        self.local_node
    }
    pub fn close(&mut self) {
        self.client = None;
    }
    /// Consumes a successfully authenticated session at a frame boundary.
    /// Guard reads leave no unread bytes in the upstream BufReader.
    pub fn into_transport(mut self) -> Result<S, Error> {
        let guard = self.client.take().ok_or(Error::State)?.into_inner();
        if guard.http.is_some() || guard.header_len != 0 || guard.remaining != 0 {
            return Err(Error::State);
        }
        Ok(guard.io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Io {
        input: io::Cursor<Vec<u8>>,
        step: usize,
    }
    impl Read for Io {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            let n = b.len().min(self.step);
            self.input.read(&mut b[..n])
        }
    }
    impl Write for Io {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl DeadlineIo for Io {
        fn set_deadline(&mut self, _: Instant) {}
    }
    fn guard(bytes: Vec<u8>, step: usize) -> Guard<Io> {
        Guard {
            io: Io {
                input: io::Cursor::new(bytes),
                step,
            },
            budget: Arc::new(Mutex::new(Budget {
                deadline: Instant::now() + Duration::from_secs(1),
                bytes: OPERATION_BYTES,
            })),
            http: Some(Vec::new()),
            header: [0; 5],
            header_len: 0,
            remaining: 0,
        }
    }

    #[test]
    fn connection_requires_authenticated_server_information_first() {
        for kind in [3, 6, 5] {
            let mut wire =
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: DERP\r\nConnection: Upgrade\r\n\r\n"
                    .to_vec();
            let mut greeting = crate::protocol::derp::MAGIC.to_vec();
            greeting.extend_from_slice(PrivateKey::<Node>::from_bytes([2; 32]).public().as_bytes());
            wire.push(1);
            wire.extend_from_slice(&(greeting.len() as u32).to_be_bytes());
            wire.extend(greeting);
            let length: usize = match kind {
                3 => 40,
                5 => 32,
                _ => 0,
            };
            wire.push(kind);
            wire.extend_from_slice(&(length as u32).to_be_bytes());
            wire.extend(vec![0; length]);
            let io = Io {
                input: io::Cursor::new(wire),
                step: 1,
            };
            assert!(
                RelaySession::connect(
                    io,
                    "relay.example",
                    PrivateKey::from_bytes([1; 32]),
                    Duration::from_secs(1)
                )
                .is_err()
            );
        }
    }

    #[test]
    fn fragmented_headers_are_bounded_before_upstream_line_allocation() {
        let valid =
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: DERP\r\nConnection: Upgrade\r\n\r\n";
        for step in [1, 2, 17, 1024] {
            let mut guard = guard(valid.to_vec(), step);
            let mut output = Vec::new();
            guard.read_to_end(&mut output).unwrap();
            assert_eq!(output, valid);
        }
        for input in [
            vec![b'x'; MAX_HEADERS + 1],
            b"HTTP/1.1 101 OK\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nUpgrade: DERP\r\nConnection: Upgrade\r\n\r\n".to_vec(),
        ] {
            assert!(guard(input, 1).read_to_end(&mut Vec::new()).is_err());
        }
    }
    #[test]
    fn frame_size_is_rejected_at_header_and_unknown_frames_consume_budget() {
        let mut guard = guard(vec![5, 0xff, 0xff, 0xff, 0xff], 1);
        guard.http = None;
        assert!(guard.read_to_end(&mut Vec::new()).is_err());
        assert_eq!(guard.io.input.position(), 5);
        let mut guard = super::tests::guard([0x7f, 0, 0, 0, 0].repeat(OPERATION_BYTES), 1024);
        guard.http = None;
        assert!(guard.read_to_end(&mut Vec::new()).is_err());
        assert_eq!(guard.io.input.position(), OPERATION_BYTES as u64);
    }
}
