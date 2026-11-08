//! Couples one authenticated HTTP/2 map response to its authorization lifetime.
use crate::{
    control_stream::{MapStream, StreamError},
    coordinator::{Clock, Coordinator, MapError},
    protocol::h2::StreamEvent,
};

#[derive(Debug, thiserror::Error)]
pub enum Error<E: std::error::Error + 'static = MapError> {
    #[error("invalid map response event order")]
    State,
    #[error("map request failed with HTTP status {0}")]
    Status(u16),
    #[error("invalid or unsupported map response headers")]
    Headers,
    #[error("map response data frame exceeds 16384 bytes")]
    Size,
    #[error(transparent)]
    Frame(#[from] StreamError<E>),
}
#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    Headers,
    Maps(usize),
    Ended,
}

/// A map target must invalidate all packet authorization on disconnect.
pub trait MapTarget {
    type Error: std::error::Error + Send + Sync + 'static;
    fn apply(&mut self, bytes: &[u8], clock: Clock) -> Result<(), Self::Error>;
    fn disconnect(&mut self);
}
impl MapTarget for Coordinator {
    type Error = MapError;
    fn apply(&mut self, bytes: &[u8], clock: Clock) -> Result<(), Self::Error> {
        Coordinator::apply(self, bytes, clock)
    }
    fn disconnect(&mut self) {
        Coordinator::disconnect(self);
    }
}
impl<S: crate::timestamp::EpochStore> MapTarget for crate::network::Network<S> {
    type Error = crate::network::Error;
    fn apply(&mut self, bytes: &[u8], clock: Clock) -> Result<(), Self::Error> {
        self.apply_map(bytes, clock)
    }
    fn disconnect(&mut self) {
        crate::network::Network::disconnect(self);
    }
}

/// Exclusively borrows the map target until dropped. Stream completion,
/// cancellation via close/drop, or any error invalidates all authorization.
pub struct MapSession<'a, T: MapTarget = Coordinator> {
    coordinator: &'a mut T,
    decoder: MapStream,
    headers: bool,
    closed: bool,
}
impl<'a, T: MapTarget> MapSession<'a, T> {
    pub fn new(coordinator: &'a mut T) -> Self {
        coordinator.disconnect();
        Self {
            coordinator,
            decoder: MapStream::default(),
            headers: false,
            closed: false,
        }
    }
    pub fn target(&mut self) -> &mut T {
        self.coordinator
    }
    pub fn close(&mut self) {
        self.decoder.close();
        self.coordinator.disconnect();
        self.closed = true;
    }
    /// Events must originate from the authenticated control connection.
    pub fn apply(&mut self, event: StreamEvent, clock: Clock) -> Result<Progress, Error<T::Error>> {
        let result = self.apply_inner(event, clock);
        if result.is_err() {
            self.close();
        }
        result
    }
    fn apply_inner(
        &mut self,
        event: StreamEvent,
        clock: Clock,
    ) -> Result<Progress, Error<T::Error>> {
        if self.closed {
            return Err(Error::State);
        }
        match event {
            StreamEvent::Headers {
                status,
                headers,
                end_stream,
            } => {
                if self.headers {
                    return Err(Error::State);
                }
                if status != 200 {
                    return Err(Error::Status(status));
                }
                if headers.len() > 64
                    || headers
                        .iter()
                        .map(|(k, v)| k.len() + v.len())
                        .sum::<usize>()
                        > 16384
                {
                    return Err(Error::Headers);
                }
                let statuses: Vec<_> = headers.iter().filter(|(k, _)| k == ":status").collect();
                if statuses.len() != 1 || statuses[0].1 != "200" {
                    return Err(Error::Headers);
                }
                let encoding: Vec<_> = headers
                    .iter()
                    .filter(|(k, _)| k.eq_ignore_ascii_case("content-encoding"))
                    .collect();
                if encoding.len() > 1
                    || encoding
                        .first()
                        .is_some_and(|(_, v)| !v.eq_ignore_ascii_case("identity"))
                {
                    return Err(Error::Headers);
                }
                self.headers = true;
                if end_stream {
                    self.close();
                    Ok(Progress::Ended)
                } else {
                    Ok(Progress::Headers)
                }
            }
            StreamEvent::Data {
                payload,
                end_stream,
            } => {
                if !self.headers {
                    return Err(Error::State);
                }
                if payload.len() > 16384 {
                    return Err(Error::Size);
                }
                let mut maps = 0;
                self.decoder.push(&payload, |bytes| {
                    self.coordinator.apply(bytes, clock)?;
                    maps += 1;
                    Ok(())
                })?;
                if end_stream {
                    self.decoder.finish()?;
                    self.close();
                    Ok(Progress::Ended)
                } else {
                    Ok(Progress::Maps(maps))
                }
            }
        }
    }
}
impl MapSession<'_, Coordinator> {
    pub fn coordinator(&mut self) -> &mut Coordinator {
        self.target()
    }
}
impl<T: MapTarget> Drop for MapSession<'_, T> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        h2::{Frame, HeaderCodec, StreamResponseAssembler},
        key::{Node, PublicKey},
    };
    use serde_json::json;
    use std::net::Ipv4Addr;
    fn key(n: u8) -> PublicKey<Node> {
        PublicKey::from_bytes([n; 32])
    }
    fn clock() -> Clock {
        Clock {
            unix_ms: 1_735_689_600_000,
            monotonic_ms: 0,
        }
    }
    fn coordinator() -> Coordinator {
        Coordinator::new(key(1), Ipv4Addr::new(100, 64, 0, 1), 4, 2, 0).unwrap()
    }
    fn full() -> Vec<u8> {
        serde_json::to_vec(&json!({"Node":{"ID":1,"Key":key(1).to_string(),"Addresses":["100.64.0.1/32"],"MachineAuthorized":true},"Peers":[{"ID":2,"Key":key(2).to_string(),"Addresses":["100.64.0.2/32"],"MachineAuthorized":true}],"PacketFilter":[]})).unwrap()
    }
    fn framed(bytes: &[u8]) -> Vec<u8> {
        let mut frame = (bytes.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(bytes);
        frame
    }
    fn headers() -> StreamEvent {
        StreamEvent::Headers {
            status: 200,
            headers: vec![(":status".into(), "200".into())],
            end_stream: false,
        }
    }
    fn allowed(c: &mut Coordinator) -> bool {
        c.data_plane().begin_handshake(key(2), 0).is_ok()
    }
    #[test]
    fn http2_data_fragmentation_updates_peers_and_eof_revokes_them() {
        let wire = framed(&full());
        for split in 0..=wire.len() {
            let mut c = coordinator();
            let mut session = MapSession::new(&mut c);
            let mut assembler = StreamResponseAssembler::new(1).unwrap();
            let mut codec = HeaderCodec::default();
            let header = assembler
                .push(Frame::new(1, 4, 1, vec![0x88]), &mut codec)
                .unwrap()
                .unwrap();
            assert_eq!(session.apply(header, clock()).unwrap(), Progress::Headers);
            for chunk in [&wire[..split], &wire[split..]] {
                let event = assembler
                    .push(Frame::new(0, 0, 1, chunk.to_vec()), &mut codec)
                    .unwrap()
                    .unwrap();
                session.apply(event, clock()).unwrap();
            }
            assert!(allowed(session.coordinator()));
            let event = assembler
                .push(Frame::new(0, 1, 1, vec![]), &mut codec)
                .unwrap()
                .unwrap();
            assert_eq!(session.apply(event, clock()).unwrap(), Progress::Ended);
            assert!(!allowed(session.coordinator()));
        }
    }
    #[test]
    fn failed_status_compression_and_event_order_never_authorize() {
        for event in [
            StreamEvent::Headers {
                status: 403,
                headers: vec![(":status".into(), "403".into())],
                end_stream: false,
            },
            StreamEvent::Headers {
                status: 200,
                headers: vec![
                    (":status".into(), "200".into()),
                    ("content-encoding".into(), "gzip".into()),
                ],
                end_stream: false,
            },
            StreamEvent::Headers {
                status: 200,
                headers: vec![
                    (":status".into(), "200".into()),
                    (":status".into(), "403".into()),
                ],
                end_stream: false,
            },
            StreamEvent::Data {
                payload: framed(&full()),
                end_stream: false,
            },
        ] {
            let mut c = coordinator();
            c.apply(&full(), clock()).unwrap();
            assert!(allowed(&mut c));
            let mut session = MapSession::new(&mut c);
            assert!(!allowed(session.coordinator()));
            assert!(session.apply(event, clock()).is_err());
            assert!(session.apply(headers(), clock()).is_err());
            assert!(!allowed(session.coordinator()));
        }
    }
    #[test]
    fn malformed_later_map_truncated_eof_and_drop_revoke_prior_maps() {
        for suffix in [framed(b"{bad"), vec![2, 0], vec![2, 0, 0, 0, b'{']] {
            let mut c = coordinator();
            let mut session = MapSession::new(&mut c);
            session.apply(headers(), clock()).unwrap();
            session
                .apply(
                    StreamEvent::Data {
                        payload: framed(&full()),
                        end_stream: false,
                    },
                    clock(),
                )
                .unwrap();
            assert!(allowed(session.coordinator()));
            assert!(
                session
                    .apply(
                        StreamEvent::Data {
                            payload: suffix,
                            end_stream: true
                        },
                        clock()
                    )
                    .is_err()
            );
            assert!(!allowed(session.coordinator()));
        }
        let mut c = coordinator();
        {
            let mut session = MapSession::new(&mut c);
            session.apply(headers(), clock()).unwrap();
            session
                .apply(
                    StreamEvent::Data {
                        payload: framed(&full()),
                        end_stream: false,
                    },
                    clock(),
                )
                .unwrap();
            assert!(allowed(session.coordinator()));
        }
        assert!(!allowed(&mut c));
    }
    #[test]
    fn oversized_data_and_repeated_headers_revoke_active_session() {
        for event in [
            headers(),
            StreamEvent::Data {
                payload: vec![0; 16385],
                end_stream: false,
            },
        ] {
            let mut c = coordinator();
            let mut session = MapSession::new(&mut c);
            session.apply(headers(), clock()).unwrap();
            session
                .apply(
                    StreamEvent::Data {
                        payload: framed(&full()),
                        end_stream: false,
                    },
                    clock(),
                )
                .unwrap();
            assert!(allowed(session.coordinator()));
            assert!(session.apply(event, clock()).is_err());
            assert!(!allowed(session.coordinator()));
        }
    }
}
