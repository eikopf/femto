//! Incremental steady-state DERP framing after authenticated server information.
use crate::{
    derp::MAX_DATAGRAM,
    protocol::{
        derp::Event,
        key::{Node, PublicKey},
    },
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid, oversized, or out-of-state DERP frame")]
    Frame,
    #[error("partial DERP frame timed out or clock moved backwards")]
    Time,
    #[error("DERP frame decoder closed")]
    Closed,
}
#[derive(Default)]
pub struct Decoder {
    header: [u8; 5],
    header_len: usize,
    body: Vec<u8>,
    length: usize,
    started: Option<u64>,
    last_ms: u64,
    closed: bool,
}
impl Decoder {
    pub fn close(&mut self) {
        self.closed = true;
        self.body.clear();
    }
    pub fn maintain(&mut self, now_ms: u64) -> Result<(), Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        if now_ms < self.last_ms
            || self
                .started
                .is_some_and(|start| now_ms.saturating_sub(start) >= 5000)
        {
            self.close();
            return Err(Error::Time);
        }
        self.last_ms = now_ms;
        Ok(())
    }
    pub fn wanted(&self) -> usize {
        if self.header_len < 5 {
            5 - self.header_len
        } else {
            self.length - self.body.len()
        }
    }
    /// Consumes at most one frame and returns its event. Unknown bounded frames
    /// consume input without an event. The caller must retain unconsumed bytes.
    pub fn feed(&mut self, bytes: &[u8], now_ms: u64) -> Result<(usize, Option<Event>), Error> {
        self.maintain(now_ms)?;
        let result = self.consume(bytes, now_ms);
        if result.is_err() {
            self.close();
        }
        result
    }
    fn consume(&mut self, mut bytes: &[u8], now_ms: u64) -> Result<(usize, Option<Event>), Error> {
        let initial = bytes.len();
        if !bytes.is_empty() {
            self.started.get_or_insert(now_ms);
        }
        if self.header_len < 5 {
            let count = bytes.len().min(5 - self.header_len);
            self.header[self.header_len..self.header_len + count].copy_from_slice(&bytes[..count]);
            self.header_len += count;
            bytes = &bytes[count..];
            if self.header_len < 5 {
                return Ok((initial - bytes.len(), None));
            }
            self.length =
                u32::from_be_bytes(self.header[1..].try_into().map_err(|_| Error::Frame)?) as usize;
            let valid = match self.header[0] {
                5 => (32..=32 + MAX_DATAGRAM).contains(&self.length),
                6 => self.length == 0,
                8 => (32..=33).contains(&self.length),
                0x12 | 0x13 | 0x15 => self.length == 8,
                0x14 => self.length <= 1024,
                1..=4 => false,
                _ => self.length <= 256,
            };
            if !valid {
                return Err(Error::Frame);
            }
        }
        let count = bytes.len().min(self.length - self.body.len());
        self.body.extend_from_slice(&bytes[..count]);
        bytes = &bytes[count..];
        if self.body.len() != self.length {
            return Ok((initial - bytes.len(), None));
        }
        let body = &self.body;
        let event = match self.header[0] {
            5 => Some(Event::Packet {
                source: PublicKey::<Node>::from_bytes(
                    body[..32].try_into().map_err(|_| Error::Frame)?,
                ),
                payload: body[32..].to_vec(),
            }),
            6 => Some(Event::KeepAlive),
            8 => Some(Event::PeerGone {
                peer: PublicKey::<Node>::from_bytes(
                    body[..32].try_into().map_err(|_| Error::Frame)?,
                ),
                reason: body.get(32).copied().unwrap_or(0),
            }),
            0x12 => Some(Event::Ping(
                body.as_slice().try_into().map_err(|_| Error::Frame)?,
            )),
            0x13 => Some(Event::Pong(
                body.as_slice().try_into().map_err(|_| Error::Frame)?,
            )),
            0x14 => Some(Event::Health(
                String::from_utf8(body.clone()).map_err(|_| Error::Frame)?,
            )),
            0x15 => Some(Event::Restarting {
                reconnect_in_ms: u32::from_be_bytes(
                    body[..4].try_into().map_err(|_| Error::Frame)?,
                ),
                try_for_ms: u32::from_be_bytes(body[4..].try_into().map_err(|_| Error::Frame)?),
            }),
            _ => None,
        };
        self.header_len = 0;
        self.body.clear();
        self.started = None;
        Ok((initial - bytes.len(), event))
    }
    pub fn eof(&mut self) -> Result<(), Error> {
        let partial = self.header_len != 0;
        self.close();
        if partial { Err(Error::Frame) } else { Ok(()) }
    }
}
fn frame(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(5 + body.len());
    bytes.push(kind);
    bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
    bytes.extend_from_slice(body);
    bytes
}
pub fn send_packet(peer: PublicKey<Node>, packet: &[u8]) -> Result<Vec<u8>, Error> {
    if packet.len() > MAX_DATAGRAM {
        return Err(Error::Frame);
    }
    let mut body = peer.as_bytes().to_vec();
    body.extend_from_slice(packet);
    Ok(frame(4, &body))
}
pub fn pong(value: [u8; 8]) -> Vec<u8> {
    frame(0x13, &value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_frames_preserve_events_and_consumption() {
        let key = PublicKey::<Node>::from_bytes([7; 32]);
        let mut packet = key.as_bytes().to_vec();
        packet.extend_from_slice(b"payload");
        let cases = vec![
            (
                frame(5, &packet),
                Some(Event::Packet {
                    source: key,
                    payload: b"payload".to_vec(),
                }),
            ),
            (frame(6, &[]), Some(Event::KeepAlive)),
            (
                frame(8, key.as_bytes()),
                Some(Event::PeerGone {
                    peer: key,
                    reason: 0,
                }),
            ),
            (frame(0x12, b"12345678"), Some(Event::Ping(*b"12345678"))),
            (pong(*b"abcdefgh"), Some(Event::Pong(*b"abcdefgh"))),
            (frame(0x14, b"health"), Some(Event::Health("health".into()))),
            (
                frame(0x15, &[0, 0, 0, 1, 0, 0, 0, 2]),
                Some(Event::Restarting {
                    reconnect_in_ms: 1,
                    try_for_ms: 2,
                }),
            ),
            (frame(0x7f, b"unknown"), None),
        ];
        for step in [1, 3, 1000] {
            for (wire, expected) in &cases {
                let mut decoder = Decoder::default();
                let mut result = None;
                for chunk in wire.chunks(step) {
                    let (used, event) = decoder.feed(chunk, 0).unwrap();
                    assert_eq!(used, chunk.len());
                    if event.is_some() {
                        assert!(result.is_none());
                        result = event;
                    }
                }
                assert_eq!(&result, expected);
                decoder.eof().unwrap();
            }
        }
        let mut decoder = Decoder::default();
        let pair = [frame(6, &[]), frame(6, &[])].concat();
        assert_eq!(decoder.feed(&pair, 0).unwrap(), (5, Some(Event::KeepAlive)));
        assert_eq!(
            decoder.feed(&pair[5..], 0).unwrap(),
            (5, Some(Event::KeepAlive))
        );
        let outbound = send_packet(key, b"payload").unwrap();
        assert_eq!(outbound[0], 4);
        assert_eq!(&outbound[5..], packet);
    }
    #[test]
    fn invalid_lengths_timeout_and_truncation_are_terminal() {
        for header in [
            [5, 255, 255, 255, 255],
            [6, 0, 0, 0, 1],
            [0x12, 0, 0, 0, 7],
            [0x14, 0, 0, 4, 1],
            [3, 0, 0, 0, 40],
            [0x7f, 0, 0, 1, 1],
        ] {
            let mut decoder = Decoder::default();
            assert!(decoder.feed(&header, 0).is_err());
            assert!(decoder.body.is_empty());
            assert!(decoder.feed(&frame(6, &[]), 0).is_err());
        }
        for wire in [vec![5], vec![5, 0, 0, 0, 32, 0]] {
            let mut decoder = Decoder::default();
            decoder.feed(&wire, 0).unwrap();
            assert!(decoder.eof().is_err());
        }
        let mut decoder = Decoder::default();
        decoder.feed(&[5], 10).unwrap();
        decoder.maintain(5009).unwrap();
        assert!(decoder.maintain(5010).is_err());
        let mut decoder = Decoder::default();
        decoder.maintain(10).unwrap();
        assert!(decoder.maintain(9).is_err());
        assert!(send_packet(PublicKey::from_bytes([1; 32]), &[0; MAX_DATAGRAM + 1]).is_err());
    }
}
