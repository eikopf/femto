//! Bounded ownership of outgoing and incoming WireGuard handshakes.
use crate::{
    plane::{DataPlane, HandshakePermit, PendingPacket, PlaneError},
    protocol::{
        key::{Node, PrivateKey, PublicKey},
        wireguard::{HandshakeInitiator, HandshakeResponder, WireGuardError},
    },
    timestamp::{EpochStore, TimestampClock},
    transport::Session,
};
use std::collections::BTreeMap;
type Key = PublicKey<Node>;
const TIMEOUT_MS: u64 = 5000;

pub enum Event {
    Response {
        peer: Key,
        packet: [u8; 92],
    },
    Established(Key),
    Transport {
        peer: Key,
        packet: Option<PendingPacket>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("pending handshake capacity reached or invalid")]
    Capacity,
    #[error("peer already has a pending handshake")]
    Pending,
    #[error("unknown or expired handshake response")]
    Unknown,
    #[error("invalid handshake response")]
    Packet,
    #[error("handshake receiver index space exhausted")]
    Index,
    #[error("durable handshake timestamp unavailable")]
    Timestamp,
    #[error("replayed handshake initiation")]
    Replay,
    #[error(transparent)]
    Transport(#[from] crate::transport::TransportError),
    #[error("monotonic clock moved backwards")]
    Clock,
    #[error(transparent)]
    Plane(#[from] PlaneError),
    #[error(transparent)]
    Protocol(#[from] WireGuardError),
}

struct Pending {
    peer: Key,
    permit: HandshakePermit,
    initiator: HandshakeInitiator,
    deadline: u64,
}
struct Responding {
    peer: Key,
    permit: HandshakePermit,
    session: Session,
    deadline: u64,
}
struct Route {
    peer: Key,
    session: std::sync::Arc<()>,
}
/// Cumulative authenticated session installations for this owner's lifetime.
/// Responder installations require an authenticated transport confirmation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Completed {
    pub initiated: u64,
    pub responded: u64,
}
pub struct Handshakes {
    key: PrivateKey<Node>,
    pending: BTreeMap<u32, Pending>,
    responding: BTreeMap<u32, Responding>,
    timestamps: BTreeMap<Key, [u8; 12]>,
    routes: BTreeMap<u32, Route>,
    capacity: usize,
    next_index: u32,
    last_ms: u64,
    completed: Completed,
}
/// Compatibility name for the former outgoing-only owner.
pub type Initiations = Handshakes;
impl Handshakes {
    /// Common WireGuard input path for UDP and relay datagrams. The underlay
    /// source is deliberately absent; it cannot supply peer authorization.
    pub fn handle(
        &mut self,
        plane: &mut DataPlane,
        packet: &[u8],
        now_ms: u64,
    ) -> Result<Event, Error> {
        let kind = packet.get(..4).ok_or(Error::Packet)?;
        match u32::from_le_bytes(kind.try_into().map_err(|_| Error::Packet)?) {
            1 => {
                let (peer, packet) = self.respond(plane, packet, now_ms)?;
                Ok(Event::Response { peer, packet })
            }
            2 => Ok(Event::Established(self.finish(plane, packet, now_ms)?)),
            4 => {
                let (peer, packet) = self.receive(plane, packet, now_ms)?;
                Ok(Event::Transport { peer, packet })
            }
            _ => Err(Error::Packet),
        }
    }
    /// One owner per node key. Receiver indices are never reused by this owner.
    pub fn new(key: PrivateKey<Node>, capacity: usize, now_ms: u64) -> Result<Self, Error> {
        if capacity == 0 || capacity > 256 {
            return Err(Error::Capacity);
        }
        Ok(Self {
            key,
            pending: BTreeMap::new(),
            responding: BTreeMap::new(),
            timestamps: BTreeMap::new(),
            routes: BTreeMap::new(),
            capacity,
            next_index: 1,
            last_ms: now_ms,
            completed: Completed::default(),
        })
    }
    /// Call on map updates, timers, disconnect, and before handling packets.
    pub fn maintain(&mut self, plane: &mut DataPlane, now_ms: u64) -> Result<(), Error> {
        if now_ms < self.last_ms {
            self.clear();
            self.routes.clear();
            plane.disconnect();
            return Err(Error::Clock);
        }
        self.last_ms = now_ms;
        self.pending.retain(|_, p| {
            p.deadline > now_ms && plane.validate_handshake(&p.permit, now_ms).is_ok()
        });
        self.responding.retain(|_, p| {
            p.deadline > now_ms && plane.validate_handshake(&p.permit, now_ms).is_ok()
        });
        self.routes
            .retain(|_, route| plane.receive_session_valid(route.peer, &route.session, now_ms));
        Ok(())
    }
    pub fn clear(&mut self) {
        self.pending.clear();
        self.responding.clear();
    }
    /// Counters saturate and survive clearing pending handshakes or disconnect.
    pub fn completed(&self) -> Completed {
        self.completed
    }
    pub fn pending_count(&self) -> usize {
        self.pending.len() + self.responding.len()
    }
    pub(crate) fn pending_for(&self, peer: Key) -> bool {
        self.pending.values().any(|p| p.peer == peer)
            || self.responding.values().any(|p| p.peer == peer)
    }
    pub fn begin<S: EpochStore>(
        &mut self,
        plane: &mut DataPlane,
        peer: Key,
        timestamps: &mut TimestampClock<S>,
        now_ms: u64,
    ) -> Result<[u8; 148], Error> {
        self.maintain(plane, now_ms)?;
        let permit = plane.begin_handshake(peer, now_ms)?;
        if self.pending.values().any(|p| p.peer == peer) {
            return Err(Error::Pending);
        }
        if self.pending_count() == self.capacity {
            return Err(Error::Capacity);
        }
        let index = self.next_index;
        self.next_index = index.checked_add(1).ok_or(Error::Index)?;
        let deadline = now_ms.checked_add(TIMEOUT_MS).ok_or(Error::Clock)?;
        let timestamp = timestamps.next_timestamp().map_err(|_| Error::Timestamp)?;
        let mut initiator = HandshakeInitiator::new(&self.key, peer, index)?;
        let packet = initiator.initiation(timestamp)?;
        self.pending.insert(
            index,
            Pending {
                peer,
                permit,
                initiator,
                deadline,
            },
        );
        Ok(packet)
    }
    /// Installs only a cryptographically verified response to a current permit.
    /// A failed cryptographic response consumes that attempt; retry uses a fresh
    /// timestamp, ephemeral key, and index. No plaintext is queued here.
    pub fn finish(
        &mut self,
        plane: &mut DataPlane,
        response: &[u8],
        now_ms: u64,
    ) -> Result<Key, Error> {
        self.maintain(plane, now_ms)?;
        if response.len() != 92 || response[..4] != 2u32.to_le_bytes() {
            return Err(Error::Packet);
        }
        let index = u32::from_le_bytes(response[8..12].try_into().map_err(|_| Error::Packet)?);
        let pending = self.pending.remove(&index).ok_or(Error::Unknown)?;
        let session = pending.initiator.finish(response)?;
        plane.install(pending.permit, Session::initiated(session, now_ms), now_ms)?;
        self.remember(plane, index, pending.peer, now_ms)?;
        self.completed.initiated = self.completed.initiated.saturating_add(1);
        Ok(pending.peer)
    }
    /// Authenticates an initiation but retains its session unconfirmed. The
    /// returned response must be sent before using confirm for its first packet.
    pub fn respond(
        &mut self,
        plane: &mut DataPlane,
        initiation: &[u8],
        now_ms: u64,
    ) -> Result<(Key, [u8; 92]), Error> {
        self.maintain(plane, now_ms)?;
        if initiation.len() != 148 || initiation[..4] != 1u32.to_le_bytes() {
            return Err(Error::Packet);
        }
        if self.pending_count() >= self.capacity {
            return Err(Error::Capacity);
        }
        let index = self.next_index;
        self.next_index = index.checked_add(1).ok_or(Error::Index)?;
        let response = HandshakeResponder::respond(&self.key, initiation, index)?;
        let peer = response.remote_public;
        let permit = plane.begin_handshake(peer, now_ms)?;
        if self
            .timestamps
            .get(&peer)
            .is_some_and(|last| response.timestamp <= *last)
        {
            return Err(Error::Replay);
        }
        if !self.timestamps.contains_key(&peer) && self.timestamps.len() == 256 {
            return Err(Error::Capacity);
        }
        let deadline = now_ms.checked_add(TIMEOUT_MS).ok_or(Error::Clock)?;
        self.timestamps.insert(peer, response.timestamp);
        self.responding.retain(|_, p| p.peer != peer);
        self.responding.insert(
            index,
            Responding {
                peer,
                permit,
                session: Session::new(response.session, now_ms),
                deadline,
            },
        );
        Ok((peer, response.packet))
    }
    /// A valid encrypted transport packet confirms the responder's keys. Failed
    /// authentication retains pending state. Policy still gates plaintext delivery.
    pub fn confirm(
        &mut self,
        plane: &mut DataPlane,
        packet: &[u8],
        now_ms: u64,
    ) -> Result<(Key, Option<PendingPacket>), Error> {
        self.maintain(plane, now_ms)?;
        if packet.len() < 32
            || packet.len() > crate::transport::TUNNEL_MTU + 32
            || packet[..4] != 4u32.to_le_bytes()
        {
            return Err(Error::Packet);
        }
        let index = u32::from_le_bytes(packet[4..8].try_into().map_err(|_| Error::Packet)?);
        let pending = self.responding.get_mut(&index).ok_or(Error::Unknown)?;
        let bytes = pending.session.decrypt(packet, now_ms)?;
        let pending = self.responding.remove(&index).ok_or(Error::Unknown)?;
        plane.install(pending.permit, pending.session, now_ms)?;
        self.remember(plane, index, pending.peer, now_ms)?;
        self.completed.responded = self.completed.responded.saturating_add(1);
        Ok((
            pending.peer,
            plane.authenticated_packet(pending.peer, bytes, now_ms)?,
        ))
    }
    fn remember(
        &mut self,
        plane: &mut DataPlane,
        index: u32,
        peer: Key,
        now_ms: u64,
    ) -> Result<(), Error> {
        let session = plane
            .session_identity(peer, now_ms)?
            .ok_or(Error::Unknown)?;
        // Each of at most 256 peers retains current and previous receive keys.
        // Insertion may have evicted an expired or superseded session.
        self.maintain(plane, now_ms)?;
        if self.routes.len() >= 512 {
            return Err(Error::Capacity);
        }
        self.routes.insert(index, Route { peer, session });
        Ok(())
    }
    /// Dispatches transport packets by receiver index, including responder key
    /// confirmation. The returned peer is meaningful only after authentication.
    pub fn receive(
        &mut self,
        plane: &mut DataPlane,
        packet: &[u8],
        now_ms: u64,
    ) -> Result<(Key, Option<PendingPacket>), Error> {
        self.maintain(plane, now_ms)?;
        if packet.len() < 32
            || packet.len() > crate::transport::TUNNEL_MTU + 32
            || packet[..4] != 4u32.to_le_bytes()
        {
            return Err(Error::Packet);
        }
        let index = u32::from_le_bytes(packet[4..8].try_into().map_err(|_| Error::Packet)?);
        if self.responding.contains_key(&index) {
            return self.confirm(plane, packet, now_ms);
        }
        let route = self.routes.get(&index).ok_or(Error::Unknown)?;
        Ok((
            route.peer,
            plane.receive_session(route.peer, &route.session, packet, now_ms)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        plane::Peer,
        protocol::{identity::DeviceIdentity, wireguard::HandshakeResponder},
    };
    use std::net::Ipv4Addr;
    #[derive(Default)]
    struct Store;
    impl EpochStore for Store {
        type Error = ();
        fn load_epoch(&mut self) -> Result<Option<u64>, ()> {
            Ok(None)
        }
        fn reserve_epoch(&mut self, _: u64) -> Result<(), ()> {
            Ok(())
        }
    }
    fn plane(peer: Key) -> DataPlane {
        let mut plane = DataPlane::new(Ipv4Addr::new(100, 64, 0, 1), 4, 2, 0).unwrap();
        plane
            .apply_update(
                true,
                &[Peer {
                    key: peer,
                    addresses: vec![Ipv4Addr::new(100, 64, 0, 2)],
                    expires_ms: 10000,
                }],
                &[],
                br#"{"PacketFilter":[]}"#,
                0,
            )
            .unwrap();
        plane
    }
    #[test]
    fn responder_requires_confirmation_and_shares_indices() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let mut pa = plane(bk);
        let mut pb = plane(ak);
        let mut ha = Handshakes::new(a.node_key().clone(), 2, 0).unwrap();
        let mut hb = Handshakes::new(b.node_key().clone(), 2, 0).unwrap();
        let mut ta = TimestampClock::open(Store, 100).unwrap();
        let mut tb = TimestampClock::open(Store, 100).unwrap();
        let initiation = ha.begin(&mut pa, bk, &mut ta, 0).unwrap();
        let (peer, response) = hb.respond(&mut pb, &initiation, 0).unwrap();
        assert_eq!(peer, ak);
        assert!(pb.send(ak, &[], 0).is_err());
        let reverse = hb.begin(&mut pb, ak, &mut tb, 0).unwrap();
        assert_ne!(&response[4..8], &reverse[4..8]);
        assert_eq!(ha.completed(), Completed::default());
        assert_eq!(hb.completed(), Completed::default());
        ha.finish(&mut pa, &response, 1).unwrap();
        assert_eq!(
            ha.completed(),
            Completed {
                initiated: 1,
                responded: 0
            }
        );
        let packet = pa.send(bk, &[], 1).unwrap();
        let mut corrupt = packet.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(hb.receive(&mut pb, &corrupt, 1).is_err());
        assert!(pb.send(ak, &[], 1).is_err());
        assert_eq!(hb.completed(), Completed::default());
        let (confirmed, payload) = hb.receive(&mut pb, &packet, 2).unwrap();
        assert_eq!(confirmed, ak);
        assert_eq!(
            hb.completed(),
            Completed {
                initiated: 0,
                responded: 1
            }
        );
        assert!(payload.is_none());
        let reply = pb.send(ak, &[], 2).unwrap();
        assert!(ha.receive(&mut pa, &reply, 2).unwrap().1.is_none());
        assert!(hb.receive(&mut pb, &packet, 2).is_err());
        pa.apply_update(false, &[], &[], b"{}", 2).unwrap();
        let reply = pb.send(ak, &[], 2).unwrap();
        assert!(ha.receive(&mut pa, &reply, 2).unwrap().1.is_none());
        hb.clear();
        assert!(matches!(
            hb.respond(&mut pb, &initiation, 3),
            Err(Error::Replay)
        ));
        let packet = pa.send(bk, &[], 3).unwrap();
        pb.disconnect();
        assert!(hb.receive(&mut pb, &packet, 3).is_err());
        assert!(hb.routes.is_empty());
        hb.clear();
        assert_eq!(
            hb.completed(),
            Completed {
                initiated: 0,
                responded: 1
            }
        );
        assert_eq!(
            ha.completed(),
            Completed {
                initiated: 1,
                responded: 0
            }
        );
    }

    #[test]
    fn index_routes_follow_session_replacement_and_expiry() {
        let local = DeviceIdentity::generate().unwrap();
        let remote = DeviceIdentity::generate().unwrap();
        let peer = remote.node_key().public();
        let mut plane = plane(peer);
        let mut owner = Handshakes::new(local.node_key().clone(), 1, 0).unwrap();
        let mut timestamps = TimestampClock::open(Store, 100).unwrap();
        let mut delayed: Vec<Vec<u8>> = Vec::new();
        for now in 0..3 {
            let initiation = owner.begin(&mut plane, peer, &mut timestamps, now).unwrap();
            let mut response =
                HandshakeResponder::respond(remote.node_key(), &initiation, 42).unwrap();
            owner.finish(&mut plane, &response.packet, now).unwrap();
            if let Some(old) = delayed.last() {
                assert!(owner.receive(&mut plane, old, now).unwrap().1.is_none());
                assert!(owner.receive(&mut plane, old, now).is_err());
            }
            if delayed.len() == 2 {
                assert!(matches!(
                    owner.receive(&mut plane, &delayed[0], now),
                    Err(Error::Unknown)
                ));
            }
            let packet = response.session.encrypt(&[]).unwrap();
            let mut unknown = packet.clone();
            unknown[4..8].copy_from_slice(&999u32.to_le_bytes());
            assert!(matches!(
                owner.receive(&mut plane, &unknown, now),
                Err(Error::Unknown)
            ));
            assert!(owner.receive(&mut plane, &packet, now).unwrap().1.is_none());
            delayed.push(response.session.encrypt(&[]).unwrap());
            assert_eq!(owner.routes.len(), (now as usize + 1).min(2));
        }
        assert!(
            owner
                .receive(&mut plane, delayed.last().unwrap(), 10000)
                .is_err()
        );
        assert!(owner.routes.is_empty());
    }

    #[test]
    fn responder_revocation_expiry_and_unauthorized_initiations() {
        for revoke in [true, false] {
            let a = DeviceIdentity::generate().unwrap();
            let b = DeviceIdentity::generate().unwrap();
            let ak = a.node_key().public();
            let mut pb = plane(ak);
            let mut hb = Handshakes::new(b.node_key().clone(), 1, 0).unwrap();
            let mut initiator =
                HandshakeInitiator::new(a.node_key(), b.node_key().public(), 88).unwrap();
            let packet = initiator
                .initiation(std::time::UNIX_EPOCH + std::time::Duration::from_secs(100))
                .unwrap();
            let (_, response) = hb.respond(&mut pb, &packet, 0).unwrap();
            let mut session = initiator.finish(&response).unwrap();
            let confirmation = session.encrypt(&[]).unwrap();
            let now = if revoke {
                pb.disconnect();
                1
            } else {
                5000
            };
            assert!(hb.confirm(&mut pb, &confirmation, now).is_err());
            assert_eq!(hb.pending_count(), 0);
            assert!(pb.send(ak, &[], now).is_err());
            if revoke {
                assert!(hb.respond(&mut pb, &packet, now).is_err());
            }
        }
    }

    #[test]
    fn response_installs_verified_session_and_cannot_be_replayed() {
        let local = DeviceIdentity::generate().unwrap();
        let remote = DeviceIdentity::generate().unwrap();
        let peer = remote.node_key().public();
        let mut plane = plane(peer);
        let mut initiations = Initiations::new(local.node_key().clone(), 2, 0).unwrap();
        let mut timestamps = TimestampClock::open(Store, 100).unwrap();
        let request = initiations
            .begin(&mut plane, peer, &mut timestamps, 0)
            .unwrap();
        let mut response = HandshakeResponder::respond(remote.node_key(), &request, 42).unwrap();
        assert_eq!(response.remote_public, local.node_key().public());
        assert_eq!(
            initiations.finish(&mut plane, &response.packet, 1).unwrap(),
            peer
        );
        let packet = plane.send(peer, &[], 1).unwrap();
        assert!(response.session.decrypt(&packet).unwrap().is_empty());
        assert_eq!(initiations.pending_count(), 0);
        assert!(matches!(
            initiations.finish(&mut plane, &response.packet, 2),
            Err(Error::Unknown)
        ));
    }
    #[test]
    fn revocation_timeout_and_clock_rollback_discard_pending_work() {
        for mode in 0..4 {
            let local = DeviceIdentity::generate().unwrap();
            let remote = DeviceIdentity::generate().unwrap();
            let peer = remote.node_key().public();
            let mut plane = plane(peer);
            let mut initiations = Initiations::new(local.node_key().clone(), 1, 0).unwrap();
            let mut timestamps = TimestampClock::open(Store, 100).unwrap();
            let request = initiations
                .begin(&mut plane, peer, &mut timestamps, 1)
                .unwrap();
            let response = HandshakeResponder::respond(remote.node_key(), &request, 42).unwrap();
            let now = match mode {
                0 => {
                    plane.apply_update(false, &[], &[peer], b"{}", 2).unwrap();
                    2
                }
                1 => 5001,
                2 => 0,
                _ => {
                    plane.apply_update(false, &[], &[], b"{}", 2).unwrap();
                    2
                }
            };
            assert!(
                initiations
                    .finish(&mut plane, &response.packet, now)
                    .is_err()
            );
            assert_eq!(initiations.pending_count(), 0);
            assert!(plane.send(peer, &[], now).is_err());
        }
    }
    #[test]
    fn pending_capacity_bad_response_and_index_nonreuse() {
        let local = DeviceIdentity::generate().unwrap();
        let remote = DeviceIdentity::generate().unwrap();
        let peer = remote.node_key().public();
        let another = DeviceIdentity::generate().unwrap().node_key().public();
        let mut plane = plane(peer);
        plane
            .apply_update(
                false,
                &[Peer {
                    key: another,
                    addresses: vec![Ipv4Addr::new(100, 64, 0, 3)],
                    expires_ms: 10000,
                }],
                &[],
                b"{}",
                0,
            )
            .unwrap();
        let mut initiations = Initiations::new(local.node_key().clone(), 1, 0).unwrap();
        let mut timestamps = TimestampClock::open(Store, 100).unwrap();
        let first = initiations
            .begin(&mut plane, peer, &mut timestamps, 0)
            .unwrap();
        assert!(matches!(
            initiations.begin(&mut plane, peer, &mut timestamps, 0),
            Err(Error::Pending)
        ));
        assert!(matches!(
            initiations.begin(&mut plane, another, &mut timestamps, 0),
            Err(Error::Capacity)
        ));
        let mut response = HandshakeResponder::respond(remote.node_key(), &first, 42).unwrap();
        response.packet[20] ^= 1;
        assert!(initiations.finish(&mut plane, &response.packet, 1).is_err());
        assert!(plane.send(peer, &[], 1).is_err());
        let second = initiations
            .begin(&mut plane, peer, &mut timestamps, 2)
            .unwrap();
        assert_ne!(&first[4..8], &second[4..8]);
        initiations.clear();
        let third = initiations
            .begin(&mut plane, peer, &mut timestamps, 3)
            .unwrap();
        assert_ne!(&second[4..8], &third[4..8]);
    }
}
