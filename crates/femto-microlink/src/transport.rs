use std::collections::BTreeMap;

use crate::protocol::{
    key::{Node, PublicKey},
    wireguard::{WireGuardError, WireGuardSession},
};

pub const SESSION_LIFETIME_MS: u64 = 180_000;
pub const SESSION_REKEY_MS: u64 = 120_000;
pub const SESSION_RECEIVE_REKEY_MS: u64 = SESSION_LIFETIME_MS - 10_000 - 5_000;
pub const SESSION_REKEY_MESSAGES: u64 = 1 << 60;
pub const TUNNEL_MTU: usize = 1280;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("session expired or monotonic clock moved backwards")]
    Expired,
    #[error("packet exceeds tunnel MTU")]
    Oversize,
    #[error("invalid IPv4 payload")]
    InvalidPacket,
    #[error("active peer session capacity reached")]
    Capacity,
    #[error(transparent)]
    Protocol(#[from] WireGuardError),
}

pub struct Session {
    wireguard: WireGuardSession,
    created_ms: u64,
    initiated: bool,
    next_send_counter: u64,
    identity: std::sync::Arc<()>,
}

impl Session {
    pub fn new(wireguard: WireGuardSession, now_ms: u64) -> Self {
        Self {
            wireguard,
            created_ms: now_ms,
            initiated: false,
            next_send_counter: 0,
            identity: std::sync::Arc::new(()),
        }
    }
    pub(crate) fn initiated(wireguard: WireGuardSession, now_ms: u64) -> Self {
        Self {
            initiated: true,
            ..Self::new(wireguard, now_ms)
        }
    }
    pub(crate) fn needs_rekey(&self, now_ms: u64) -> bool {
        self.next_send_counter > SESSION_REKEY_MESSAGES
            || self.initiated
                && now_ms
                    .checked_sub(self.created_ms)
                    .is_some_and(|age| age >= SESSION_REKEY_MS)
    }
    pub(crate) fn identity(&self) -> std::sync::Arc<()> {
        self.identity.clone()
    }
    pub(crate) fn needs_receive_rekey(&self, now_ms: u64) -> bool {
        self.initiated
            && now_ms
                .checked_sub(self.created_ms)
                .is_some_and(|age| age > SESSION_RECEIVE_REKEY_MS)
    }
    pub fn expired(&self, now_ms: u64) -> bool {
        now_ms
            .checked_sub(self.created_ms)
            .is_none_or(|elapsed| elapsed >= SESSION_LIFETIME_MS)
    }
    pub fn encrypt(&mut self, packet: &[u8], now_ms: u64) -> Result<Vec<u8>, TransportError> {
        if self.expired(now_ms) {
            return Err(TransportError::Expired);
        }
        if packet.len() > TUNNEL_MTU {
            return Err(TransportError::Oversize);
        }
        if !packet.is_empty() && ipv4_length(packet)? != packet.len() {
            return Err(TransportError::InvalidPacket);
        }
        let encrypted = self.wireguard.encrypt(packet)?;
        // The pinned transport emits its actual nonce counter in this header.
        // Observe it rather than maintaining a second cryptographic counter.
        self.next_send_counter = u64::from_le_bytes(
            encrypted[8..16]
                .try_into()
                .expect("WireGuard transport header"),
        )
        .saturating_add(1);
        Ok(encrypted)
    }
    /// Returns an authenticated IPv4 packet or an empty keepalive. Receive policy
    /// and peer source-address checks must precede delivery to an application.
    pub fn decrypt(&mut self, packet: &[u8], now_ms: u64) -> Result<Vec<u8>, TransportError> {
        if self.expired(now_ms) {
            return Err(TransportError::Expired);
        }
        if packet.len() > TUNNEL_MTU + 32 {
            return Err(TransportError::Oversize);
        }
        let mut plaintext = self.wireguard.decrypt(packet)?;
        if plaintext.is_empty() {
            return Ok(plaintext);
        }
        let length = ipv4_length(&plaintext)?;
        plaintext.truncate(length);
        Ok(plaintext)
    }
}

fn ipv4_length(packet: &[u8]) -> Result<usize, TransportError> {
    if packet.len() < 20 || packet[0] != 0x45 {
        return Err(TransportError::InvalidPacket);
    }
    let length = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    // No IP options or fragments in the initial IPv4 data plane.
    if length < 20
        || length > packet.len()
        || u16::from_be_bytes([packet[6], packet[7]]) & 0x3fff != 0
    {
        return Err(TransportError::InvalidPacket);
    }
    Ok(length)
}

/// Bounds active cryptographic state independently of the coordinator directory.
pub struct SessionPool {
    capacity: usize,
    sessions: BTreeMap<PublicKey<Node>, Session>,
    previous: BTreeMap<PublicKey<Node>, Session>,
}

impl SessionPool {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            sessions: BTreeMap::new(),
            previous: BTreeMap::new(),
        }
    }
    pub fn insert(
        &mut self,
        peer: PublicKey<Node>,
        session: Session,
        now_ms: u64,
    ) -> Result<(), TransportError> {
        self.sessions.retain(|_, session| !session.expired(now_ms));
        self.previous
            .retain(|peer, session| self.sessions.contains_key(peer) && !session.expired(now_ms));
        if !self.sessions.contains_key(&peer) && self.sessions.len() >= self.capacity {
            return Err(TransportError::Capacity);
        }
        if let Some(previous) = self.sessions.insert(peer, session) {
            self.previous.insert(peer, previous);
        }
        Ok(())
    }
    pub(crate) fn by_identity(
        &mut self,
        peer: &PublicKey<Node>,
        identity: &std::sync::Arc<()>,
        now_ms: u64,
    ) -> Option<&mut Session> {
        self.sessions
            .get_mut(peer)
            .into_iter()
            .chain(self.previous.get_mut(peer))
            .find(|session| {
                !session.expired(now_ms) && std::sync::Arc::ptr_eq(&session.identity, identity)
            })
    }
    pub fn get_mut(&mut self, peer: &PublicKey<Node>) -> Option<&mut Session> {
        self.sessions.get_mut(peer)
    }
    pub fn revoke(&mut self, peer: &PublicKey<Node>) {
        self.sessions.remove(peer);
        self.previous.remove(peer);
    }
    pub fn clear(&mut self) {
        self.sessions.clear();
        self.previous.clear();
    }
    pub fn retain_authorized(&mut self, mut authorized: impl FnMut(&PublicKey<Node>) -> bool) {
        self.sessions.retain(|key, _| authorized(key));
        self.previous
            .retain(|key, _| self.sessions.contains_key(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        identity::DeviceIdentity,
        wireguard::{HandshakeInitiator, HandshakeResponder},
    };
    use std::time::{Duration, UNIX_EPOCH};

    fn pair() -> (Session, Session) {
        let alice = DeviceIdentity::generate().unwrap();
        let bob = DeviceIdentity::generate().unwrap();
        let mut handshake =
            HandshakeInitiator::new(alice.node_key(), bob.node_key().public(), 1).unwrap();
        let initiation = handshake
            .initiation(UNIX_EPOCH + Duration::from_secs(100))
            .unwrap();
        let response = HandshakeResponder::respond(bob.node_key(), &initiation, 2).unwrap();
        (
            Session::new(handshake.finish(&response.packet).unwrap(), 1000),
            Session::new(response.session, 1000),
        )
    }

    #[test]
    fn authenticated_roundtrip_replay_and_tamper() {
        let (mut alice, mut bob) = pair();
        let mut ip = vec![0; 20];
        ip[0] = 0x45;
        ip[3] = 20;
        let packet = alice.encrypt(&ip, 1001).unwrap();
        let mut forged = packet.clone();
        *forged.last_mut().unwrap() ^= 1;
        assert!(bob.decrypt(&forged, 1001).is_err());
        assert_eq!(bob.decrypt(&packet, 1001).unwrap(), ip);
        assert!(bob.decrypt(&packet, 1001).is_err());
        let keepalive = alice.encrypt(&[], 1001).unwrap();
        assert!(bob.decrypt(&keepalive, 1001).unwrap().is_empty());
        assert!(bob.decrypt(&keepalive, 1001).is_err());
    }
    #[test]
    fn message_count_renewal_applies_to_both_roles_and_uses_emitted_counter() {
        for initiated in [false, true] {
            let (mut session, _) = pair();
            session.initiated = initiated;
            for count in [SESSION_REKEY_MESSAGES - 1, SESSION_REKEY_MESSAGES] {
                session.next_send_counter = count;
                assert!(!session.needs_rekey(1001));
            }
            session.next_send_counter = SESSION_REKEY_MESSAGES + 1;
            assert!(session.needs_rekey(1001));
            assert!(session.encrypt(&[0; 4], 1001).is_err());
            assert_eq!(session.next_send_counter, SESSION_REKEY_MESSAGES + 1);
        }
        let (session, _) = pair();
        let mut wireguard = session.wireguard;
        // Wrapping a previously used transport must observe its nonce, not zero.
        wireguard.encrypt(&[]).unwrap();
        wireguard.encrypt(&[]).unwrap();
        let mut session = Session::new(wireguard, 1000);
        let encrypted = session.encrypt(&[], 1001).unwrap();
        assert_eq!(u64::from_le_bytes(encrypted[8..16].try_into().unwrap()), 2);
        assert_eq!(session.next_send_counter, 3);
        session.encrypt(&[], 1002).unwrap();
        assert_eq!(session.next_send_counter, 4);
        let (fresh, _) = pair();
        assert!(!fresh.needs_rekey(1001));
    }

    #[test]
    fn previous_receive_keys_keep_original_expiry_and_capacity_bounds() {
        let identity = DeviceIdentity::generate().unwrap();
        let peer = identity.node_key().public();
        let other = DeviceIdentity::generate().unwrap().node_key().public();
        let mut pool = SessionPool::new(1);
        let (first, _) = pair();
        let first_id = first.identity();
        pool.insert(peer, first, 1000).unwrap();
        let (mut second, _) = pair();
        second.created_ms = 2000;
        let second_id = second.identity();
        pool.insert(peer, second, 2000).unwrap();
        assert!(pool.by_identity(&peer, &first_id, 180999).is_some());
        assert!(pool.by_identity(&peer, &first_id, 181000).is_none());
        assert!(pool.by_identity(&peer, &second_id, 181000).is_some());
        let (mut third, _) = pair();
        third.created_ms = 181000;
        let third_id = third.identity();
        pool.insert(peer, third, 181000).unwrap();
        assert!(pool.by_identity(&peer, &first_id, 181000).is_none());
        assert!(pool.by_identity(&peer, &second_id, 181000).is_some());
        assert_eq!(pool.sessions.len(), 1);
        assert_eq!(pool.previous.len(), 1);
        let (mut extra, _) = pair();
        extra.created_ms = 181000;
        assert!(matches!(
            pool.insert(other, extra, 181000),
            Err(TransportError::Capacity)
        ));
        pool.retain_authorized(|_| false);
        assert!(pool.by_identity(&peer, &second_id, 181000).is_none());
        assert!(pool.by_identity(&peer, &third_id, 181000).is_none());
        assert!(pool.sessions.is_empty());
        assert!(pool.previous.is_empty());
    }

    #[test]
    fn delayed_receive_and_clock_rollback_reject_keys() {
        let (mut alice, mut bob) = pair();
        let packet = alice.encrypt(&[], 1001).unwrap();
        assert!(matches!(
            bob.decrypt(&packet, 181_000),
            Err(TransportError::Expired)
        ));
        assert!(matches!(
            alice.encrypt(&[], 999),
            Err(TransportError::Expired)
        ));
    }
}
