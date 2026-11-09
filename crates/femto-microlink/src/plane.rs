//! Single-owner policy and session boundary for authenticated control updates.
use crate::{
    policy::{PolicyError, ReceivePolicy},
    protocol::{
        key::{Node, PublicKey},
        netmap::parse_ip_packet,
    },
    transport::{Session, SessionPool, TransportError},
};
use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr},
};

type Key = PublicKey<Node>;
const MAX_UDP_REPLIES: usize = 32;
const UDP_REPLY_LIFETIME_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub key: Key,
    pub addresses: Vec<Ipv4Addr>,
    /// Monotonic deadline translated by the authenticated control adapter.
    pub expires_ms: u64,
    /// This peer may reply to managed outgoing flows but cannot initiate inbound traffic.
    pub jailed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum PlaneError {
    #[error("unauthorized, expired, or unknown peer")]
    Unauthorized,
    #[error("invalid or oversized peer update")]
    Directory,
    #[error("authorization generation is stale")]
    Stale,
    #[error("monotonic clock moved backwards")]
    Clock,
    #[error("invalid or unsupported transport packet")]
    Packet,
    #[error(transparent)]
    Policy(#[from] PolicyError),
    #[error(transparent)]
    Transport(#[from] TransportError),
}

/// Must accompany completion of the handshake for this peer. Fields are private
/// so callers cannot manufacture a permit for an unknown directory entry.
#[derive(Clone)]
pub struct HandshakePermit {
    peer: Key,
    generation: std::sync::Arc<()>,
}

/// Decrypted data remains opaque until release checks current authorization.
pub struct PendingPacket {
    peer: Key,
    generation: std::sync::Arc<()>,
    bytes: Vec<u8>,
}
impl PendingPacket {
    pub(crate) fn capacity(&self) -> usize {
        self.bytes.capacity()
    }

    pub(crate) fn tcp_endpoints(&self) -> Option<(std::net::SocketAddrV4, std::net::SocketAddrV4)> {
        use smoltcp::wire::{Ipv4Packet, TcpPacket};
        if self.bytes.get(9) != Some(&6) {
            return None;
        }
        let ip = Ipv4Packet::new_checked(&self.bytes[..]).ok()?;
        let tcp = TcpPacket::new_checked(ip.payload()).ok()?;
        Some((
            std::net::SocketAddrV4::new(ip.src_addr(), tcp.src_port()),
            std::net::SocketAddrV4::new(ip.dst_addr(), tcp.dst_port()),
        ))
    }

    pub(crate) fn udp_destination(&self) -> Result<Option<u16>, crate::udp::Error> {
        if self.bytes.get(9) != Some(&17) {
            return Ok(None);
        }
        Ok(Some(crate::udp::decode(&self.bytes)?.destination.port()))
    }
}

struct TcpReply {
    peer: Key,
    local: std::net::SocketAddrV4,
    remote: std::net::SocketAddrV4,
    lease: std::sync::Weak<()>,
}
pub(crate) struct UdpReply {
    peer: Key,
    socket: crate::udp::Socket,
    local: std::net::SocketAddrV4,
    remote: std::net::SocketAddrV4,
    deadline: u64,
}

pub struct DataPlane {
    local: Ipv4Addr,
    max_peers: usize,
    peers: BTreeMap<Key, Peer>,
    policy: ReceivePolicy,
    sessions: SessionPool,
    generation: std::sync::Arc<()>,
    authorization: std::sync::Arc<()>,
    last_ms: u64,
    synchronized: bool,
    tcp_replies: Vec<TcpReply>,
    udp_replies: Vec<UdpReply>,
}

impl DataPlane {
    pub(crate) fn authorization_identity(&self) -> std::sync::Arc<()> {
        self.authorization.clone()
    }

    #[cfg(test)]
    pub(crate) fn generation(&self) -> std::sync::Arc<()> {
        self.generation.clone()
    }
    pub fn new(
        local: Ipv4Addr,
        max_peers: usize,
        max_sessions: usize,
        now_ms: u64,
    ) -> Result<Self, PlaneError> {
        if max_peers == 0 || max_peers > 256 || max_sessions == 0 || max_sessions > max_peers {
            return Err(PlaneError::Directory);
        }
        Ok(Self {
            local,
            max_peers,
            peers: BTreeMap::new(),
            policy: ReceivePolicy::default(),
            sessions: SessionPool::new(max_sessions),
            generation: std::sync::Arc::new(()),
            authorization: std::sync::Arc::new(()),
            last_ms: now_ms,
            synchronized: false,
            tcp_replies: Vec::new(),
            udp_replies: Vec::new(),
        })
    }

    /// Invalidates queued packets and unfinished handshakes, including on sleep.
    pub fn disconnect(&mut self) {
        self.authorization = std::sync::Arc::new(());
        self.tcp_replies.clear();
        self.udp_replies.clear();
        self.generation = std::sync::Arc::new(());
        self.peers.clear();
        self.policy.reset();
        self.sessions.clear();
        self.synchronized = false;
    }

    /// Retire expired transport keys even when no application traffic arrives.
    /// Owners must poll this with their monotonic clock while retaining the plane.
    pub fn maintain(&mut self, now_ms: u64) -> Result<(), PlaneError> {
        self.clock(now_ms)?;
        self.sessions.retire_expired(now_ms);
        Ok(())
    }

    fn clock(&mut self, now_ms: u64) -> Result<(), PlaneError> {
        let previous = self.last_ms;
        self.last_ms = now_ms;
        if now_ms < previous {
            self.disconnect();
            return Err(PlaneError::Clock);
        }
        Ok(())
    }

    fn peer(&self, key: &Key, now_ms: u64) -> Result<&Peer, PlaneError> {
        self.peers
            .get(key)
            .filter(|peer| self.synchronized && now_ms < peer.expires_ms)
            .ok_or(PlaneError::Unauthorized)
    }

    /// The caller must authenticate the coordinator before calling this method.
    /// Full updates reset omitted policy to deny; deltas preserve omitted fields.
    /// Any invalid update clears authorization and requires a fresh full update.
    pub fn apply_update(
        &mut self,
        full: bool,
        upserts: &[Peer],
        removed: &[Key],
        map: &[u8],
        now_ms: u64,
    ) -> Result<(), PlaneError> {
        self.clock(now_ms)?;
        let result = self.prepare(full, upserts, removed, map, now_ms);
        let (peers, policy) = match result {
            Ok(result) => result,
            Err(error) => {
                self.disconnect();
                return Err(error);
            }
        };
        if !self.synchronized || peers != self.peers || !policy.same_rules(&self.policy) {
            self.udp_replies.clear();
            self.authorization = std::sync::Arc::new(());
        }
        self.sessions.retain_authorized(|key| {
            peers
                .get(key)
                .is_some_and(|peer| self.peers.get(key) == Some(peer) && peer.expires_ms > now_ms)
        });
        self.peers = peers;
        self.policy = policy;
        self.tcp_replies.clear();
        self.generation = std::sync::Arc::new(());
        self.synchronized = true;
        Ok(())
    }

    fn prepare(
        &self,
        full: bool,
        upserts: &[Peer],
        removed: &[Key],
        map: &[u8],
        now_ms: u64,
    ) -> Result<(BTreeMap<Key, Peer>, ReceivePolicy), PlaneError> {
        if (!full && !self.synchronized)
            || upserts.len() > self.max_peers
            || removed.len() > self.max_peers
            || (full && !removed.is_empty())
        {
            return Err(PlaneError::Directory);
        }
        let mut peers = if full {
            BTreeMap::new()
        } else {
            self.peers.clone()
        };
        let mut seen = std::collections::BTreeSet::new();
        for key in removed {
            if !seen.insert(*key) {
                return Err(PlaneError::Directory);
            }
            peers.remove(key);
        }
        for peer in upserts {
            if !seen.insert(peer.key) || peer.addresses.is_empty() || peer.addresses.len() > 4 {
                return Err(PlaneError::Directory);
            }
            if peer.expires_ms <= now_ms {
                peers.remove(&peer.key);
            } else {
                peers.insert(peer.key, peer.clone());
            }
        }
        if peers.len() > self.max_peers {
            return Err(PlaneError::Directory);
        }
        let mut addresses = std::collections::BTreeSet::new();
        for peer in peers.values() {
            for address in &peer.addresses {
                if *address == self.local
                    || address.is_unspecified()
                    || address.is_multicast()
                    || address.is_broadcast()
                    || address.is_loopback()
                    || !addresses.insert(*address)
                {
                    return Err(PlaneError::Directory);
                }
            }
        }
        let mut policy = if full {
            ReceivePolicy::default()
        } else {
            self.policy.clone()
        };
        policy.apply_map(map)?;
        Ok((peers, policy))
    }

    pub fn begin_handshake(
        &mut self,
        peer: Key,
        now_ms: u64,
    ) -> Result<HandshakePermit, PlaneError> {
        self.clock(now_ms)?;
        self.peer(&peer, now_ms)?;
        Ok(HandshakePermit {
            peer,
            generation: self.generation.clone(),
        })
    }

    /// Rechecks a pending handshake against current map generation and expiry.
    pub fn validate_handshake(
        &mut self,
        permit: &HandshakePermit,
        now_ms: u64,
    ) -> Result<(), PlaneError> {
        self.clock(now_ms)?;
        if !std::sync::Arc::ptr_eq(&permit.generation, &self.generation) {
            return Err(PlaneError::Stale);
        }
        self.peer(&permit.peer, now_ms)?;
        Ok(())
    }

    /// Session must be produced by a handshake authenticated to the permit peer.
    pub fn install(
        &mut self,
        permit: HandshakePermit,
        session: Session,
        now_ms: u64,
    ) -> Result<(), PlaneError> {
        self.validate_handshake(&permit, now_ms)?;
        if session.expired(now_ms) {
            return Err(TransportError::Expired.into());
        }
        self.sessions.insert(permit.peer, session, now_ms)?;
        Ok(())
    }

    /// Identifies one currently authorized installed session for index routing.
    pub(crate) fn session_identity(
        &mut self,
        peer: Key,
        now_ms: u64,
    ) -> Result<Option<std::sync::Arc<()>>, PlaneError> {
        self.clock(now_ms)?;
        if self.peer(&peer, now_ms).is_err() {
            return Ok(None);
        }
        Ok(self
            .sessions
            .get_mut(&peer)
            .filter(|session| !session.expired(now_ms))
            .map(|session| session.identity()))
    }

    pub(crate) fn session_needs_rekey(
        &mut self,
        peer: Key,
        now_ms: u64,
    ) -> Result<bool, PlaneError> {
        self.clock(now_ms)?;
        self.peer(&peer, now_ms)?;
        Ok(self
            .sessions
            .get_mut(&peer)
            .is_some_and(|session| !session.expired(now_ms) && session.needs_rekey(now_ms)))
    }

    pub(crate) fn session_needs_receive_rekey(
        &mut self,
        peer: Key,
        now_ms: u64,
    ) -> Result<bool, PlaneError> {
        self.clock(now_ms)?;
        self.peer(&peer, now_ms)?;
        Ok(self
            .sessions
            .get_mut(&peer)
            .is_some_and(|session| !session.expired(now_ms) && session.needs_receive_rekey(now_ms)))
    }

    pub(crate) fn receive_session_valid(
        &mut self,
        peer: Key,
        identity: &std::sync::Arc<()>,
        now_ms: u64,
    ) -> bool {
        self.clock(now_ms).is_ok()
            && self.peer(&peer, now_ms).is_ok()
            && self.sessions.by_identity(&peer, identity, now_ms).is_some()
    }

    pub(crate) fn receive_session(
        &mut self,
        peer: Key,
        identity: &std::sync::Arc<()>,
        ciphertext: &[u8],
        now_ms: u64,
    ) -> Result<Option<PendingPacket>, PlaneError> {
        self.clock(now_ms)?;
        self.peer(&peer, now_ms)?;
        let bytes = self
            .sessions
            .by_identity(&peer, identity, now_ms)
            .ok_or(PlaneError::Unauthorized)?
            .decrypt(ciphertext, now_ms)?;
        self.authenticated_packet(peer, bytes, now_ms)
    }

    /// Direct UDP and DERP must both enter through this same receive path.
    pub fn receive(
        &mut self,
        peer: Key,
        ciphertext: &[u8],
        now_ms: u64,
    ) -> Result<Option<PendingPacket>, PlaneError> {
        self.clock(now_ms)?;
        self.peer(&peer, now_ms)?;
        let bytes = self
            .sessions
            .get_mut(&peer)
            .ok_or(PlaneError::Unauthorized)?
            .decrypt(ciphertext, now_ms)?;
        self.authenticated_packet(peer, bytes, now_ms)
    }

    /// Internal boundary for plaintext authenticated by a newly confirmed session.
    pub(crate) fn authenticated_packet(
        &mut self,
        peer: Key,
        bytes: Vec<u8>,
        now_ms: u64,
    ) -> Result<Option<PendingPacket>, PlaneError> {
        self.clock(now_ms)?;
        self.peer(&peer, now_ms)?;
        if bytes.is_empty() {
            return Ok(None);
        }
        self.authorize(&peer, &bytes, now_ms)?;
        Ok(Some(PendingPacket {
            peer,
            generation: self.generation.clone(),
            bytes,
        }))
    }

    pub fn send(&mut self, peer: Key, bytes: &[u8], now_ms: u64) -> Result<Vec<u8>, PlaneError> {
        self.validate_outgoing(peer, bytes, now_ms)?;
        Ok(self
            .sessions
            .get_mut(&peer)
            .ok_or(PlaneError::Unauthorized)?
            .encrypt(bytes, now_ms)?)
    }
    /// Install return permission only for a managed flow with queued outbound traffic.
    pub(crate) fn allow_tcp_reply(
        &mut self,
        permit: &HandshakePermit,
        local: std::net::SocketAddrV4,
        remote: std::net::SocketAddrV4,
        lease: &std::sync::Arc<()>,
        now_ms: u64,
    ) -> Result<(), PlaneError> {
        self.validate_handshake(permit, now_ms)?;
        self.validate_remote_address(permit.peer, *remote.ip(), now_ms)?;
        if *local.ip() != self.local || local.port() == 0 || remote.port() == 0 {
            return Err(PlaneError::Unauthorized);
        }
        self.tcp_replies
            .retain(|flow| flow.lease.strong_count() > 0);
        if self
            .tcp_replies
            .iter()
            .any(|flow| flow.lease.ptr_eq(&std::sync::Arc::downgrade(lease)))
        {
            return Ok(());
        }
        if self.tcp_replies.len() >= 4 {
            return Err(PlaneError::Directory);
        }
        self.tcp_replies.push(TcpReply {
            peer: permit.peer,
            local,
            remote,
            lease: std::sync::Arc::downgrade(lease),
        });
        Ok(())
    }
    /// Prepare before queuing outbound ciphertext; commit only after queue acceptance.
    pub(crate) fn prepare_udp_reply(
        &mut self,
        peer: Key,
        socket: crate::udp::Socket,
        local: std::net::SocketAddrV4,
        remote: std::net::SocketAddrV4,
        now_ms: u64,
    ) -> Result<UdpReply, PlaneError> {
        self.validate_remote_address(peer, *remote.ip(), now_ms)?;
        if *local.ip() != self.local || local.port() == 0 || remote.port() == 0 {
            return Err(PlaneError::Unauthorized);
        }
        let deadline = now_ms
            .checked_add(UDP_REPLY_LIFETIME_MS)
            .ok_or(PlaneError::Clock)?;
        self.udp_replies.retain(|flow| flow.deadline > now_ms);
        if self.udp_replies.len() >= MAX_UDP_REPLIES
            && !self.udp_replies.iter().any(|flow| {
                flow.peer == peer
                    && flow.socket == socket
                    && flow.local == local
                    && flow.remote == remote
            })
        {
            return Err(PlaneError::Directory);
        }
        Ok(UdpReply {
            peer,
            socket,
            local,
            remote,
            deadline,
        })
    }
    pub(crate) fn commit_udp_reply(&mut self, reply: UdpReply) -> Result<(), PlaneError> {
        if let Some(flow) = self.udp_replies.iter_mut().find(|flow| {
            flow.peer == reply.peer
                && flow.socket == reply.socket
                && flow.local == reply.local
                && flow.remote == reply.remote
        }) {
            *flow = reply;
        } else {
            // The single network owner does not prepare another flow between
            // reservation and commit; send_ip cannot add UDP return permissions.
            if self.udp_replies.len() >= MAX_UDP_REPLIES {
                return Err(PlaneError::Directory);
            }
            self.udp_replies.push(reply);
        }
        Ok(())
    }
    pub(crate) fn revoke_udp_replies(&mut self, socket: crate::udp::Socket) {
        self.udp_replies.retain(|flow| flow.socket != socket);
    }
    fn allows_udp_reply(&self, peer: Key, bytes: &[u8], now_ms: u64) -> bool {
        let Ok(packet) = crate::udp::decode(bytes) else {
            return false;
        };
        self.udp_replies.iter().any(|flow| {
            flow.peer == peer
                && flow.deadline > now_ms
                && flow.local == packet.destination
                && flow.remote == packet.source
        })
    }
    fn allows_tcp_reply(&self, peer: Key, bytes: &[u8]) -> bool {
        use smoltcp::wire::{Ipv4Packet, TcpPacket};
        let Ok(ip) = Ipv4Packet::new_checked(bytes) else {
            return false;
        };
        if bytes.get(9) != Some(&6) || !ip.verify_checksum() {
            return false;
        }
        let Ok(tcp) = TcpPacket::new_checked(ip.payload()) else {
            return false;
        };
        if (!tcp.ack() && !tcp.rst())
            || (tcp.syn() && (!tcp.ack() || tcp.rst()))
            || !tcp.verify_checksum(&ip.src_addr().into(), &ip.dst_addr().into())
        {
            return false;
        }
        self.tcp_replies.iter().any(|flow| {
            flow.peer == peer
                && flow.lease.strong_count() > 0
                && flow.local == std::net::SocketAddrV4::new(ip.dst_addr(), tcp.dst_port())
                && flow.remote == std::net::SocketAddrV4::new(ip.src_addr(), tcp.src_port())
        })
    }
    pub(crate) fn validate_remote_address(
        &mut self,
        peer: Key,
        address: Ipv4Addr,
        now_ms: u64,
    ) -> Result<(), PlaneError> {
        self.clock(now_ms)?;
        if !self.peer(&peer, now_ms)?.addresses.contains(&address) {
            return Err(PlaneError::Unauthorized);
        }
        Ok(())
    }
    pub(crate) fn validate_outgoing(
        &mut self,
        peer: Key,
        bytes: &[u8],
        now_ms: u64,
    ) -> Result<(), PlaneError> {
        self.clock(now_ms)?;
        let owner = self.peer(&peer, now_ms)?;
        if !bytes.is_empty() {
            let (source, destination, _, _) = packet_tuple(bytes)?;
            if source != self.local || !owner.addresses.contains(&destination) {
                return Err(PlaneError::Unauthorized);
            }
        }
        Ok(())
    }

    /// Call at the application-stack boundary, after any asynchronous queue.
    pub fn release(&mut self, packet: PendingPacket, now_ms: u64) -> Result<Vec<u8>, PlaneError> {
        self.clock(now_ms)?;
        if !std::sync::Arc::ptr_eq(&packet.generation, &self.generation) {
            return Err(PlaneError::Stale);
        }
        self.authorize(&packet.peer, &packet.bytes, now_ms)?;
        Ok(packet.bytes)
    }

    fn authorize(&self, peer: &Key, bytes: &[u8], now_ms: u64) -> Result<(), PlaneError> {
        let owner = self.peer(peer, now_ms)?;
        let (source, destination, protocol, port) = packet_tuple(bytes)?;
        if !owner.addresses.contains(&source)
            || destination != self.local
            || ((owner.jailed || !self.policy.allows(source, destination, protocol, port))
                && !self.allows_tcp_reply(*peer, bytes)
                && !self.allows_udp_reply(*peer, bytes, now_ms))
        {
            return Err(PlaneError::Unauthorized);
        }
        Ok(())
    }
}

fn packet_tuple(bytes: &[u8]) -> Result<(Ipv4Addr, Ipv4Addr, u8, u16), PlaneError> {
    let packet = parse_ip_packet(bytes).map_err(|_| PlaneError::Packet)?;
    let (IpAddr::V4(source), IpAddr::V4(destination)) = (packet.source, packet.destination) else {
        return Err(PlaneError::Packet);
    };
    if bytes[0] != 0x45
        || packet.fragmented
        || packet.packet.len() != bytes.len()
        || bytes[6] & 0x80 != 0
    {
        return Err(PlaneError::Packet);
    }
    let payload = packet.payload;
    let port = match packet.protocol {
        6 if payload.len() >= 20
            && (payload[12] >> 4) >= 5
            && (payload[12] as usize >> 4) * 4 <= payload.len() =>
        {
            packet.transport_ports().ok_or(PlaneError::Packet)?.1
        }
        17 if payload.len() >= 8
            && u16::from_be_bytes([payload[4], payload[5]]) as usize == payload.len() =>
        {
            packet.transport_ports().ok_or(PlaneError::Packet)?.1
        }
        1 if payload.len() >= 8 => 0,
        _ => return Err(PlaneError::Packet),
    };
    Ok((source, destination, packet.protocol, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        identity::DeviceIdentity,
        wireguard::{HandshakeInitiator, HandshakeResponder},
    };
    use std::time::{Duration, UNIX_EPOCH};
    const ALLOW: &[u8] = br#"{"PacketFilter":[{"SrcIPs":["100.64.0.2"],"DstPorts":[{"IP":"100.64.0.1","Ports":{"First":1234,"Last":1234}}],"IPProto":[17]}]}"#;
    fn pair() -> (Key, Session, Session) {
        let remote = DeviceIdentity::generate().unwrap();
        let local = DeviceIdentity::generate().unwrap();
        let mut handshake =
            HandshakeInitiator::new(remote.node_key(), local.node_key().public(), 1).unwrap();
        let initiation = handshake
            .initiation(UNIX_EPOCH + Duration::from_secs(100))
            .unwrap();
        let response = HandshakeResponder::respond(local.node_key(), &initiation, 2).unwrap();
        (
            remote.node_key().public(),
            Session::new(handshake.finish(&response.packet).unwrap(), 1000),
            Session::new(response.session, 1000),
        )
    }
    fn peer(key: Key) -> Peer {
        Peer {
            key,
            addresses: vec![Ipv4Addr::new(100, 64, 0, 2)],
            expires_ms: 10_000,
            jailed: false,
        }
    }
    fn plane() -> DataPlane {
        DataPlane::new(Ipv4Addr::new(100, 64, 0, 1), 4, 1, 1000).unwrap()
    }
    fn udp() -> Vec<u8> {
        let mut bytes = vec![0; 28];
        bytes[0] = 0x45;
        bytes[3] = 28;
        bytes[8] = 64;
        bytes[9] = 17;
        bytes[12..16].copy_from_slice(&[100, 64, 0, 2]);
        bytes[16..20].copy_from_slice(&[100, 64, 0, 1]);
        bytes[20..22].copy_from_slice(&9000u16.to_be_bytes());
        bytes[22..24].copy_from_slice(&1234u16.to_be_bytes());
        bytes[25] = 8;
        bytes
    }
    fn installed() -> (DataPlane, Key, Session) {
        let (key, remote, local) = pair();
        let mut plane = plane();
        plane
            .apply_update(true, &[peer(key)], &[], ALLOW, 1000)
            .unwrap();
        let permit = plane.begin_handshake(key, 1000).unwrap();
        plane.install(permit, local, 1000).unwrap();
        (plane, key, remote)
    }

    #[test]
    fn encrypted_receive_checks_source_destination_port_and_packet_structure() {
        let (mut plane, key, mut remote) = installed();
        let packet = udp();
        let encrypted = remote.encrypt(&packet, 1001).unwrap();
        let pending = plane.receive(key, &encrypted, 1001).unwrap().unwrap();
        assert_eq!(plane.release(pending, 1001).unwrap(), packet);
        assert!(plane.receive(key, &encrypted, 1001).is_err());
        for (index, value) in [(15, 3), (19, 9), (23, 0), (25, 9), (9, 6)] {
            let mut bad = udp();
            bad[index] = value;
            let encrypted = remote.encrypt(&bad, 1001).unwrap();
            assert!(plane.receive(key, &encrypted, 1001).is_err(), "{index}");
        }
        let keepalive = remote.encrypt(&[], 1001).unwrap();
        assert!(plane.receive(key, &keepalive, 1001).unwrap().is_none());
        let mut outgoing = udp();
        outgoing[12..20].rotate_left(4);
        let encrypted = plane.send(key, &outgoing, 1001).unwrap();
        assert_eq!(remote.decrypt(&encrypted, 1001).unwrap(), outgoing);
        assert!(plane.send(key, &udp(), 1001).is_err());
    }

    #[test]
    fn permits_and_packets_cannot_cross_plane_instances() {
        let (mut first, key, mut remote) = installed();
        let mut second = plane();
        second
            .apply_update(true, &[peer(key)], &[], ALLOW, 1001)
            .unwrap();
        let permit = first.begin_handshake(key, 1001).unwrap();
        let (_, _, session) = pair();
        assert!(matches!(
            second.install(permit, session, 1001),
            Err(PlaneError::Stale)
        ));
        let encrypted = remote.encrypt(&udp(), 1001).unwrap();
        let pending = first.receive(key, &encrypted, 1001).unwrap().unwrap();
        assert!(matches!(
            second.release(pending, 1001),
            Err(PlaneError::Stale)
        ));
    }

    #[test]
    fn session_capacity_is_distinct_from_directory_and_removal_frees_it() {
        let (mut plane, first, _) = installed();
        let (second, _, local) = pair();
        let mut record = peer(second);
        record.addresses = vec![Ipv4Addr::new(100, 64, 0, 3)];
        plane
            .apply_update(false, &[record], &[], b"{}", 1001)
            .unwrap();
        let permit = plane.begin_handshake(second, 1001).unwrap();
        assert!(matches!(
            plane.install(permit, local, 1001),
            Err(PlaneError::Transport(TransportError::Capacity))
        ));
        plane
            .apply_update(false, &[], &[first], b"{}", 1002)
            .unwrap();
        assert!(plane.sessions.get_mut(&first).is_none());
        assert!(plane.begin_handshake(second, 1002).is_ok());
        let (third, _, local) = pair();
        let mut record = peer(third);
        record.addresses = vec![Ipv4Addr::new(100, 64, 0, 4)];
        plane
            .apply_update(false, &[record], &[], b"{}", 1002)
            .unwrap();
        let permit = plane.begin_handshake(third, 1002).unwrap();
        plane.install(permit, local, 1002).unwrap();
    }

    #[test]
    fn peer_expiry_is_checked_again_after_a_packet_waits_in_a_queue() {
        let (mut plane, key, mut remote) = installed();
        let encrypted = remote.encrypt(&udp(), 1001).unwrap();
        let pending = plane.receive(key, &encrypted, 1001).unwrap().unwrap();
        assert!(matches!(
            plane.release(pending, 10_000),
            Err(PlaneError::Unauthorized)
        ));
    }

    #[test]
    fn revocation_invalidates_queued_plaintext_and_unfinished_handshakes() {
        let (mut plane, key, mut remote) = installed();
        let encrypted = remote.encrypt(&udp(), 1001).unwrap();
        let pending = plane.receive(key, &encrypted, 1001).unwrap().unwrap();
        let permit = plane.begin_handshake(key, 1001).unwrap();
        plane.apply_update(false, &[], &[key], b"{}", 1002).unwrap();
        assert!(matches!(
            plane.release(pending, 1002),
            Err(PlaneError::Stale)
        ));
        let (_, _, session) = pair();
        assert!(matches!(
            plane.install(permit, session, 1002),
            Err(PlaneError::Stale)
        ));
        assert!(plane.begin_handshake(key, 1002).is_err());
        assert!(plane.receive(key, &encrypted, 1002).is_err());
    }

    #[test]
    fn previous_session_obeys_current_policy_release_and_revocation() {
        let (mut plane, key, mut remote) = installed();
        let previous = plane.session_identity(key, 1000).unwrap().unwrap();
        let (_, replacement, _) = pair();
        let permit = plane.begin_handshake(key, 1001).unwrap();
        plane.install(permit, replacement, 1001).unwrap();
        let encrypted = remote.encrypt(&udp(), 1001).unwrap();
        let pending = plane
            .receive_session(key, &previous, &encrypted, 1001)
            .unwrap()
            .unwrap();
        plane
            .apply_update(false, &[], &[], br#"{"PacketFilter":[]}"#, 1002)
            .unwrap();
        assert!(plane.release(pending, 1002).is_err());
        let encrypted = remote.encrypt(&udp(), 1002).unwrap();
        assert!(matches!(
            plane.receive_session(key, &previous, &encrypted, 1002),
            Err(PlaneError::Unauthorized)
        ));
        plane.apply_update(false, &[], &[], ALLOW, 1003).unwrap();
        let encrypted = remote.encrypt(&udp(), 1003).unwrap();
        assert!(
            plane
                .receive_session(key, &previous, &encrypted, 1003)
                .unwrap()
                .is_some()
        );
        plane.apply_update(false, &[], &[key], b"{}", 1004).unwrap();
        assert!(!plane.receive_session_valid(key, &previous, 1004));
        let encrypted = remote.encrypt(&udp(), 1004).unwrap();
        assert!(
            plane
                .receive_session(key, &previous, &encrypted, 1004)
                .is_err()
        );
    }

    #[test]
    fn policy_updates_apply_to_live_sessions_and_full_omission_denies() {
        let (mut plane, key, mut remote) = installed();
        plane
            .apply_update(false, &[], &[], br#"{"PacketFilter":[]}"#, 1001)
            .unwrap();
        let encrypted = remote.encrypt(&udp(), 1002).unwrap();
        assert!(matches!(
            plane.receive(key, &encrypted, 1002),
            Err(PlaneError::Unauthorized)
        ));
        plane.apply_update(false, &[], &[], ALLOW, 1003).unwrap();
        let encrypted = remote.encrypt(&udp(), 1003).unwrap();
        assert!(plane.receive(key, &encrypted, 1003).unwrap().is_some());
        plane
            .apply_update(true, &[peer(key)], &[], b"{}", 1004)
            .unwrap();
        let encrypted = remote.encrypt(&udp(), 1004).unwrap();
        assert!(matches!(
            plane.receive(key, &encrypted, 1004),
            Err(PlaneError::Unauthorized)
        ));
    }

    #[test]
    fn invalid_map_duplicate_addresses_sleep_expiry_and_clock_rollback_fail_closed() {
        let (mut plane, key, mut remote) = installed();
        let other = DeviceIdentity::generate().unwrap().node_key().public();
        assert!(
            plane
                .apply_update(false, &[peer(other)], &[], b"{}", 1001)
                .is_err()
        );
        assert!(plane.begin_handshake(key, 1001).is_err());
        assert!(
            plane
                .apply_update(false, &[peer(key)], &[], ALLOW, 1001)
                .is_err()
        );
        plane
            .apply_update(true, &[peer(key)], &[], ALLOW, 1001)
            .unwrap();
        assert!(plane.apply_update(false, &[], &[], b"{", 1001).is_err());
        assert!(plane.begin_handshake(key, 1001).is_err());
        plane
            .apply_update(true, &[peer(key)], &[], ALLOW, 1001)
            .unwrap();
        assert!(plane.begin_handshake(key, 10_000).is_err());
        assert!(matches!(
            plane.begin_handshake(key, 1002),
            Err(PlaneError::Clock)
        ));
        assert!(plane.begin_handshake(key, 1003).is_err());
        plane
            .apply_update(true, &[peer(key)], &[], ALLOW, 1003)
            .unwrap();
        plane.disconnect();
        let encrypted = remote.encrypt(&[], 1003).unwrap();
        assert!(plane.receive(key, &encrypted, 1003).is_err());
    }
    fn syn_ack() -> (std::net::SocketAddrV4, std::net::SocketAddrV4, Vec<u8>) {
        let local = "100.64.0.1:49152".parse().unwrap();
        let remote = "100.64.0.2:80".parse().unwrap();
        let mut client = crate::tcp::Stream::connect(local, remote, 1).unwrap();
        let mut server = crate::tcp::Stream::new(remote, local, 2, true).unwrap();
        client.poll(0).unwrap();
        server.ingest(&client.take_packet().unwrap()).unwrap();
        server.poll(0).unwrap();
        (local, remote, server.take_packet().unwrap())
    }
    #[test]
    fn tcp_return_permission_is_exact_checksummed_and_lease_bound() {
        let (mut plane, key, mut sender) = installed();
        let (local, remote, packet) = syn_ack();
        assert!(
            plane
                .authenticated_packet(key, packet.clone(), 1001)
                .is_err()
        );
        let permit = plane.begin_handshake(key, 1001).unwrap();
        let lease = std::sync::Arc::new(());
        plane
            .allow_tcp_reply(&permit, local, remote, &lease, 1001)
            .unwrap();
        let encrypted = sender.encrypt(&packet, 1001).unwrap();
        let pending = plane.receive(key, &encrypted, 1001).unwrap().unwrap();
        assert_eq!(plane.release(pending, 1001).unwrap(), packet);
        for alteration in 0..6 {
            let mut bad = packet.clone();
            let mut tcp = smoltcp::wire::TcpPacket::new_unchecked(&mut bad[20..]);
            match alteration {
                0 => tcp.set_src_port(81),
                1 => tcp.set_dst_port(49153),
                2 => tcp.set_ack(false), // Bare SYN is not a reply.
                3 => tcp.set_rst(true),  // SYN+RST is not a reply.
                _ => (),
            }
            tcp.fill_checksum(&(*remote.ip()).into(), &(*local.ip()).into());
            if alteration == 4 {
                bad[36] ^= 1;
            }
            if alteration == 5 {
                bad[10] ^= 1;
            }
            assert!(
                plane.authenticated_packet(key, bad, 1001).is_err(),
                "{alteration}"
            );
        }
        let pending = plane
            .authenticated_packet(key, packet.clone(), 1001)
            .unwrap()
            .unwrap();
        drop(lease);
        assert!(plane.release(pending, 1001).is_err());
        assert!(plane.authenticated_packet(key, packet, 1001).is_err());
    }
    #[test]
    fn tcp_return_permission_cannot_survive_map_or_peer_expiry() {
        let (mut plane, key, _) = installed();
        let (local, remote, packet) = syn_ack();
        let permit = plane.begin_handshake(key, 1001).unwrap();
        let lease = std::sync::Arc::new(());
        plane
            .allow_tcp_reply(&permit, local, remote, &lease, 1001)
            .unwrap();
        let pending = plane
            .authenticated_packet(key, packet.clone(), 1001)
            .unwrap()
            .unwrap();
        plane.apply_update(false, &[], &[], br#"{}"#, 1002).unwrap();
        assert!(plane.release(pending, 1002).is_err());
        assert!(
            plane
                .authenticated_packet(key, packet.clone(), 1002)
                .is_err()
        );
        assert!(
            plane
                .allow_tcp_reply(&permit, local, remote, &lease, 1002)
                .is_err()
        );
        let permit = plane.begin_handshake(key, 1002).unwrap();
        plane
            .allow_tcp_reply(&permit, local, remote, &lease, 1002)
            .unwrap();
        let pending = plane
            .authenticated_packet(key, packet.clone(), 1002)
            .unwrap()
            .unwrap();
        assert!(plane.release(pending, 10000).is_err());
        assert!(plane.authenticated_packet(key, packet, 10000).is_err());
    }
    #[test]
    fn authorization_identity_preserves_identical_rules_but_not_expiry_or_invalid_updates() {
        let (mut plane, key, _) = installed();
        let authorization = plane.authorization_identity();
        let generation = plane.generation();
        plane.apply_update(false, &[], &[], br#"{}"#, 1001).unwrap();
        assert!(std::sync::Arc::ptr_eq(
            &authorization,
            &plane.authorization_identity()
        ));
        assert!(!std::sync::Arc::ptr_eq(&generation, &plane.generation()));
        plane.apply_update(false, &[], &[], ALLOW, 1001).unwrap();
        assert!(std::sync::Arc::ptr_eq(
            &authorization,
            &plane.authorization_identity()
        ));
        let mut changed = peer(key);
        changed.expires_ms -= 1;
        plane
            .apply_update(false, &[changed], &[], b"{}", 1001)
            .unwrap();
        assert!(!std::sync::Arc::ptr_eq(
            &authorization,
            &plane.authorization_identity()
        ));
        let authorization = plane.authorization_identity();
        assert!(
            plane
                .apply_update(false, &[], &[], b"invalid", 1001)
                .is_err()
        );
        assert!(!std::sync::Arc::ptr_eq(
            &authorization,
            &plane.authorization_identity()
        ));
    }
    fn jailed_plane() -> (DataPlane, Key, Peer) {
        let key = Key::from_bytes([2; 32]);
        let mut owner = peer(key);
        owner.jailed = true;
        owner.expires_ms = u64::MAX;
        let mut plane = DataPlane::new("100.64.0.1".parse().unwrap(), 4, 2, 0).unwrap();
        plane
            .apply_update(true, &[owner.clone()], &[], JAILED_ALLOW, 0)
            .unwrap();
        (plane, key, owner)
    }
    const JAILED_ALLOW: &[u8] = br#"{"PacketFilter":[{"SrcIPs":["100.64.0.2"],"DstPorts":[{"IP":"100.64.0.1","Ports":{"First":1,"Last":65535}}],"IPProto":[6,17]}]}"#;

    #[test]
    fn jailed_tcp_allows_only_leased_outbound_replies() {
        let (mut plane, key, _) = jailed_plane();
        let (local, remote, reply) = syn_ack();
        assert!(plane.authenticated_packet(key, reply.clone(), 1).is_err());
        let permit = plane.begin_handshake(key, 1).unwrap();
        let lease = std::sync::Arc::new(());
        plane
            .allow_tcp_reply(&permit, local, remote, &lease, 1)
            .unwrap();
        let pending = plane
            .authenticated_packet(key, reply.clone(), 1)
            .unwrap()
            .unwrap();
        assert_eq!(plane.release(pending, 1).unwrap(), reply);
        let mut inbound = crate::tcp::Stream::connect(remote, local, 1).unwrap();
        inbound.poll(0).unwrap();
        assert!(
            plane
                .authenticated_packet(key, inbound.take_packet().unwrap(), 2)
                .is_err()
        );
        let pending = plane.authenticated_packet(key, reply, 2).unwrap().unwrap();
        drop(lease);
        assert!(plane.release(pending, 2).is_err());
    }

    #[test]
    fn jailed_udp_permissions_require_commit_exact_tuple_and_live_deadline() {
        let (mut plane, key, _) = jailed_plane();
        let local = "100.64.0.1:1234".parse().unwrap();
        let remote = "100.64.0.2:9000".parse().unwrap();
        let mut bindings = crate::udp::Bindings::default();
        let socket = bindings.bind(1234).unwrap();
        let bytes = crate::udp::encode(remote, local, b"reply").unwrap();
        let permit = plane
            .prepare_udp_reply(key, socket, local, remote, 1)
            .unwrap();
        assert!(plane.authenticated_packet(key, bytes.clone(), 1).is_err());
        plane.commit_udp_reply(permit).unwrap();
        assert!(plane.authenticated_packet(key, bytes.clone(), 1).is_ok());
        for (from, to) in [
            ("100.64.0.2:9001", "100.64.0.1:1234"),
            ("100.64.0.2:9000", "100.64.0.1:1235"),
        ] {
            let wrong =
                crate::udp::encode(from.parse().unwrap(), to.parse().unwrap(), b"unsolicited")
                    .unwrap();
            assert!(plane.authenticated_packet(key, wrong, 2).is_err());
        }
        let mut corrupt = bytes.clone();
        corrupt[26] ^= 1;
        assert!(plane.authenticated_packet(key, corrupt, 3).is_err());
        let pending = plane
            .authenticated_packet(key, bytes.clone(), 30_000)
            .unwrap()
            .unwrap();
        assert!(plane.release(pending, 30_001).is_err());
        assert!(plane.authenticated_packet(key, bytes, 30_001).is_err());
    }

    #[test]
    fn udp_reply_permissions_are_bounded_and_revoked_on_close_and_policy_change() {
        let (mut plane, key, owner) = jailed_plane();
        let local = "100.64.0.1:1234".parse().unwrap();
        let mut bindings = crate::udp::Bindings::default();
        let socket = bindings.bind(1234).unwrap();
        for port in 9000..9032 {
            let remote = std::net::SocketAddrV4::new("100.64.0.2".parse().unwrap(), port);
            let permit = plane
                .prepare_udp_reply(key, socket, local, remote, 1)
                .unwrap();
            plane.commit_udp_reply(permit).unwrap();
        }
        let remote = "100.64.0.2:9000".parse().unwrap();
        let extra = "100.64.0.2:9032".parse().unwrap();
        assert!(
            plane
                .prepare_udp_reply(key, socket, local, extra, 2)
                .is_err()
        );
        let refresh = plane
            .prepare_udp_reply(key, socket, local, remote, 2)
            .unwrap();
        plane.commit_udp_reply(refresh).unwrap();
        assert_eq!(plane.udp_replies.len(), 32);
        plane.apply_update(false, &[], &[], b"{}", 3).unwrap();
        let reply = crate::udp::encode(remote, local, b"reply").unwrap();
        let pending = plane
            .authenticated_packet(key, reply.clone(), 3)
            .unwrap()
            .unwrap();
        plane.revoke_udp_replies(socket);
        assert!(plane.release(pending, 3).is_err());
        let permit = plane
            .prepare_udp_reply(key, socket, local, remote, 4)
            .unwrap();
        plane.commit_udp_reply(permit).unwrap();
        let pending = plane
            .authenticated_packet(key, reply.clone(), 4)
            .unwrap()
            .unwrap();
        plane
            .apply_update(false, &[owner], &[], br#"{"PacketFilter":[]}"#, 5)
            .unwrap();
        assert!(plane.release(pending, 5).is_err());
        assert!(plane.authenticated_packet(key, reply, 5).is_err());
        assert!(plane.udp_replies.is_empty());
        for port in 9000..9032 {
            let destination = std::net::SocketAddrV4::new(*remote.ip(), port);
            let permit = plane
                .prepare_udp_reply(key, socket, local, destination, 6)
                .unwrap();
            plane.commit_udp_reply(permit).unwrap();
        }
        let permit = plane
            .prepare_udp_reply(key, socket, local, extra, 30_006)
            .unwrap();
        plane.commit_udp_reply(permit).unwrap();
        let old_reply = crate::udp::encode(remote, local, b"expired").unwrap();
        assert!(plane.authenticated_packet(key, old_reply, 30_006).is_err());
        let new_reply = crate::udp::encode(extra, local, b"new flow").unwrap();
        assert!(plane.authenticated_packet(key, new_reply, 30_006).is_ok());
    }
}
