//! Bounded unauthenticated STUN observations; these never authorize a peer.
use crate::protocol::stun::{binding_request, parse_binding_response};
use std::{collections::BTreeMap, net::SocketAddrV4};
const LIMIT: usize = 8;
const TIMEOUT_MS: u64 = 5000;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid STUN server or observed address")]
    Address,
    #[error("STUN request already pending or capacity reached")]
    Capacity,
    #[error("STUN random transaction identifier unavailable")]
    Random,
    #[error("unknown, expired, or mismatched STUN response")]
    Unknown,
    #[error("invalid STUN response framing or attributes")]
    Packet,
    #[error("STUN monotonic clock moved backwards or overflowed")]
    Clock,
    #[error(transparent)]
    Protocol(#[from] crate::protocol::stun::StunError),
}
struct Pending {
    server: SocketAddrV4,
    started: u64,
    deadline: u64,
}
/// Periodic observations from at most eight authenticated-directory servers.
/// Observed endpoints are unauthenticated hints, never peer authorization.
#[derive(Default)]
pub struct EndpointDiscovery {
    transactions: Discovery,
    servers: Vec<SocketAddrV4>,
    observations: BTreeMap<SocketAddrV4, (SocketAddrV4, u64)>,
    cursor: usize,
    next_ms: u64,
}
impl EndpointDiscovery {
    pub fn clear(&mut self) {
        self.transactions.clear();
        self.servers.clear();
        self.observations.clear();
        self.cursor = 0;
        self.next_ms = 0;
    }
    pub fn configure(&mut self, servers: &[SocketAddrV4]) -> Result<(), Error> {
        if servers.len() > LIMIT {
            return Err(Error::Capacity);
        }
        if servers.iter().any(|s| {
            s.port() == 0
                || s.ip().is_unspecified()
                || s.ip().is_multicast()
                || s.ip().is_broadcast()
        }) {
            return Err(Error::Address);
        }
        let mut servers = servers.to_vec();
        servers.sort_unstable();
        servers.dedup();
        if servers != self.servers {
            self.clear();
            self.servers = servers;
        }
        Ok(())
    }
    pub fn maintain(&mut self, now_ms: u64) -> Result<(), Error> {
        if let Err(error) = self.transactions.maintain(now_ms) {
            self.clear();
            return Err(error);
        }
        self.observations
            .retain(|_, (_, deadline)| *deadline > now_ms);
        Ok(())
    }
    /// One request per second within a round, then a 30-second pause.
    /// Failed socket sends consume the attempt and retry in a later round.
    pub fn request(&mut self, now_ms: u64) -> Result<Option<(SocketAddrV4, [u8; 40])>, Error> {
        self.maintain(now_ms)?;
        if self.servers.is_empty() || now_ms < self.next_ms {
            return Ok(None);
        }
        let next = (self.cursor + 1) % self.servers.len();
        let deadline = now_ms
            .checked_add(if next == 0 { 30000 } else { 1000 })
            .ok_or(Error::Clock)?;
        let server = self.servers[self.cursor];
        let packet = self.transactions.begin(server, now_ms)?;
        self.cursor = next;
        self.next_ms = deadline;
        Ok(Some((server, packet)))
    }
    pub fn receive(
        &mut self,
        server: SocketAddrV4,
        bytes: &[u8],
        now_ms: u64,
    ) -> Result<(), Error> {
        self.maintain(now_ms)?;
        let deadline = now_ms.checked_add(60000).ok_or(Error::Clock)?;
        let observation = self.transactions.finish(server, bytes, now_ms)?;
        self.observations
            .insert(server, (observation.address, deadline));
        Ok(())
    }
    pub fn endpoints(&mut self, now_ms: u64) -> Result<Vec<SocketAddrV4>, Error> {
        self.maintain(now_ms)?;
        let mut endpoints: Vec<_> = self
            .observations
            .values()
            .map(|(address, _)| *address)
            .collect();
        endpoints.sort_unstable();
        endpoints.dedup();
        Ok(endpoints)
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct Observation {
    pub address: SocketAddrV4,
    pub server: SocketAddrV4,
    pub round_trip_ms: u64,
}
#[derive(Default)]
pub struct Discovery {
    pending: BTreeMap<[u8; 12], Pending>,
    last_ms: u64,
}
impl Discovery {
    pub fn clear(&mut self) {
        self.pending.clear();
    }
    pub fn maintain(&mut self, now_ms: u64) -> Result<(), Error> {
        if now_ms < self.last_ms {
            self.clear();
            return Err(Error::Clock);
        }
        self.last_ms = now_ms;
        self.pending.retain(|_, p| p.deadline > now_ms);
        Ok(())
    }
    /// The caller selects the server from trusted configuration or a validated
    /// DERP map, then transmits this request through the WireGuard UDP socket.
    pub fn begin(&mut self, server: SocketAddrV4, now_ms: u64) -> Result<[u8; 40], Error> {
        self.maintain(now_ms)?;
        if server.port() == 0
            || server.ip().is_unspecified()
            || server.ip().is_multicast()
            || server.ip().is_broadcast()
        {
            return Err(Error::Address);
        }
        if self.pending.len() == LIMIT || self.pending.values().any(|p| p.server == server) {
            return Err(Error::Capacity);
        }
        let mut id = [0; 12];
        getrandom::getrandom(&mut id).map_err(|_| Error::Random)?;
        if self.pending.contains_key(&id) {
            return Err(Error::Random);
        }
        let deadline = now_ms.checked_add(TIMEOUT_MS).ok_or(Error::Clock)?;
        self.pending.insert(
            id,
            Pending {
                server,
                started: now_ms,
                deadline,
            },
        );
        Ok(binding_request(id))
    }
    pub fn finish(
        &mut self,
        server: SocketAddrV4,
        packet: &[u8],
        now_ms: u64,
    ) -> Result<Observation, Error> {
        self.maintain(now_ms)?;
        if !(20..=1536).contains(&packet.len()) {
            return Err(Error::Packet);
        }
        let id: [u8; 12] = packet[8..20].try_into().map_err(|_| Error::Packet)?;
        let pending = self
            .pending
            .get(&id)
            .filter(|p| p.server == server)
            .ok_or(Error::Unknown)?;
        validate(packet)?;
        let address = parse_binding_response(packet, id)?;
        if address.port() == 0
            || address.ip().is_unspecified()
            || address.ip().is_loopback()
            || address.ip().is_multicast()
            || address.ip().is_broadcast()
        {
            return Err(Error::Address);
        }
        let observation = Observation {
            address,
            server,
            round_trip_ms: now_ms - pending.started,
        };
        self.pending.remove(&id);
        Ok(observation)
    }
}
fn validate(packet: &[u8]) -> Result<(), Error> {
    let length = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
    if length % 4 != 0 || packet.len() != 20 + length {
        return Err(Error::Packet);
    }
    let mut offset = 20;
    let mut mapped = false;
    while offset < packet.len() {
        let header = packet.get(offset..offset + 4).ok_or(Error::Packet)?;
        let kind = u16::from_be_bytes([header[0], header[1]]);
        let size = usize::from(u16::from_be_bytes([header[2], header[3]]));
        let end = offset + 4 + ((size + 3) & !3);
        if end > packet.len() {
            return Err(Error::Packet);
        }
        match kind {
            0x0020 | 0x8020 => {
                if mapped {
                    return Err(Error::Packet);
                }
                mapped = true;
            }
            0x8028 => {
                if size != 4 || end != packet.len() {
                    return Err(Error::Packet);
                }
                let expected = (crc32fast::hash(&packet[..offset]) ^ 0x5354554e).to_be_bytes();
                if packet[offset + 4..end] != expected {
                    return Err(Error::Packet);
                }
            }
            0x0001 => (),
            required if required < 0x8000 => return Err(Error::Packet),
            _ => (),
        }
        offset = end;
    }
    if !mapped {
        return Err(Error::Packet);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn server() -> SocketAddrV4 {
        "192.0.2.1:3478".parse().unwrap()
    }
    fn response(request: &[u8]) -> Vec<u8> {
        let mut bytes = vec![1, 1, 0, 12, 0x21, 0x12, 0xa4, 0x42];
        bytes.extend_from_slice(&request[8..20]);
        bytes.extend_from_slice(&[0, 32, 0, 8, 0, 1]);
        bytes.extend_from_slice(&(40000u16 ^ 0x2112).to_be_bytes());
        for (a, b) in [203, 0, 113, 9].into_iter().zip([0x21, 0x12, 0xa4, 0x42]) {
            bytes.push(a ^ b);
        }
        bytes
    }
    #[test]
    fn periodic_observations_expire_and_configuration_changes_clear_state() {
        let mut discovery = EndpointDiscovery::default();
        let other: SocketAddrV4 = "192.0.2.2:3478".parse().unwrap();
        discovery.configure(&[server(), other]).unwrap();
        let (first, request) = discovery.request(0).unwrap().unwrap();
        assert_eq!(first, server());
        assert!(discovery.request(999).unwrap().is_none());
        discovery
            .receive(server(), &response(&request), 999)
            .unwrap();
        assert_eq!(discovery.endpoints(999).unwrap().len(), 1);
        // Reordering an unchanged server set preserves the outstanding round.
        discovery.configure(&[other, server()]).unwrap();
        let (second, request) = discovery.request(1000).unwrap().unwrap();
        assert_eq!(second, other);
        discovery.receive(other, &response(&request), 1001).unwrap();
        assert_eq!(discovery.endpoints(1001).unwrap().len(), 1);
        assert!(discovery.request(30999).unwrap().is_none());
        let (_, request) = discovery.request(31000).unwrap().unwrap();
        discovery.configure(&[other]).unwrap();
        assert!(discovery.endpoints(31000).unwrap().is_empty());
        assert!(
            discovery
                .receive(server(), &response(&request), 31000)
                .is_err()
        );
        let (_, request) = discovery.request(31000).unwrap().unwrap();
        discovery
            .receive(other, &response(&request), 31001)
            .unwrap();
        assert_eq!(discovery.endpoints(91000).unwrap().len(), 1);
        assert!(discovery.endpoints(91001).unwrap().is_empty());
        assert!(discovery.configure(&[other; 9]).is_err());
        assert!(discovery.request(91000).is_err());
        assert!(discovery.endpoints(91001).unwrap().is_empty());
        assert!(discovery.request(91001).unwrap().is_none());
    }
    #[test]
    fn matching_timeout_replay_and_clock() {
        let mut d = Discovery::default();
        let request = d.begin(server(), 0).unwrap();
        let wire = response(&request);
        let mut wrong_id = wire.clone();
        wrong_id[8] ^= 1;
        assert!(d.finish(server(), &wrong_id, 0).is_err());
        assert!(matches!(d.begin(server(), 0), Err(Error::Capacity)));
        assert!(
            d.finish("192.0.2.2:3478".parse().unwrap(), &wire, 1)
                .is_err()
        );
        let result = d.finish(server(), &wire, 7).unwrap();
        assert_eq!(result.address.to_string(), "203.0.113.9:40000");
        assert_eq!(result.round_trip_ms, 7);
        assert!(d.finish(server(), &wire, 7).is_err());
        let request = d.begin(server(), 8).unwrap();
        assert!(d.finish(server(), &response(&request), 5008).is_err());
        d.begin(server(), 5008).unwrap();
        assert!(d.maintain(5007).is_err());
        assert!(d.pending.is_empty());
    }
    #[test]
    fn strict_framing_fingerprint_and_capacity() {
        let mut d = Discovery::default();
        let request = d.begin(server(), 0).unwrap();
        let good = response(&request);
        let mut trailing = good.clone();
        trailing.extend_from_slice(&[0; 4]);
        assert!(d.finish(server(), &trailing, 0).is_err());
        let mut duplicate = good.clone();
        duplicate[3] = 24;
        duplicate.extend_from_slice(&good[20..]);
        assert!(d.finish(server(), &duplicate, 0).is_err());
        let mut fingerprint = good.clone();
        fingerprint[3] = 20;
        fingerprint.extend_from_slice(&[0x80, 0x28, 0, 4]);
        let crc = crc32fast::hash(&fingerprint[..32]) ^ 0x5354554e;
        fingerprint.extend_from_slice(&crc.to_be_bytes());
        let mut bad = fingerprint.clone();
        bad[39] ^= 1;
        assert!(d.finish(server(), &bad, 0).is_err());
        assert!(d.finish(server(), &fingerprint, 0).is_ok());
        for port in 1..=8 {
            d.begin(SocketAddrV4::new(*server().ip(), port), 1).unwrap();
        }
        assert!(matches!(d.begin(server(), 1), Err(Error::Capacity)));
        d.clear();
        assert!(d.begin(server(), 1).is_ok());
    }
}
