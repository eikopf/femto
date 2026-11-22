//! Authenticated direct-path discovery bound to current coordinator authority.
use crate::{
    coordinator::{Coordinator, MapError},
    plane::{HandshakePermit, PlaneError},
    protocol::{
        disco,
        key::{Disco, Node, PrivateKey, PublicKey},
    },
};
use std::{collections::BTreeMap, net::SocketAddr};
type Key = PublicKey<Node>;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid discovery packet, endpoint, or key binding")]
    Invalid,
    #[error("unknown, expired, or mismatched discovery transaction")]
    Unknown,
    #[error("discovery capacity reached")]
    Capacity,
    #[error("discovery clock moved backwards or overflowed")]
    Clock,
    #[error("discovery random identifier unavailable")]
    Random,
    #[error(transparent)]
    Map(#[from] MapError),
    #[error(transparent)]
    Plane(#[from] PlaneError),
    #[error(transparent)]
    Protocol(#[from] disco::DiscoError),
}
struct Path {
    peer: Key,
    disco: PublicKey<Disco>,
    endpoint: SocketAddr,
    permit: HandshakePermit,
    deadline: u64,
}
struct Candidates {
    disco: PublicKey<Disco>,
    endpoints: Vec<SocketAddr>,
    permit: HandshakePermit,
    deadline: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Direct(SocketAddr),
    Relay(u64),
}
pub struct Probe {
    pub endpoint: SocketAddr,
    pub packet: Vec<u8>,
}
/// Probes sent half a second apart to a peer without a direct path before
/// the pace drops to one in five seconds. Twenty cover the ten seconds a
/// peer can take to learn this node's endpoints from the coordinator; a path
/// that has not formed by then is not about to. Five seconds is the lifetime
/// of a pending probe, the pace a peer with one endpoint gets anyway.
const BRISK_PROBES: u32 = 20;
#[derive(Default)]
struct ProbeCursor {
    index: usize,
    next_ms: u64,
    /// Probes sent since a path was last validated.
    unanswered: u32,
}
pub struct Discovery {
    local_node: Key,
    local_disco: PrivateKey<Disco>,
    pending: BTreeMap<[u8; 12], Path>,
    paths: BTreeMap<Key, Path>,
    candidates: BTreeMap<Key, Candidates>,
    probes: BTreeMap<Key, ProbeCursor>,
    next_probe_ms: u64,
    last_ms: u64,
}
impl Discovery {
    pub fn new(local_node: Key, local_disco: PrivateKey<Disco>) -> Result<Self, Error> {
        if local_node == Key::zero() {
            return Err(Error::Invalid);
        }
        Ok(Self {
            local_node,
            local_disco,
            pending: BTreeMap::new(),
            paths: BTreeMap::new(),
            candidates: BTreeMap::new(),
            probes: BTreeMap::new(),
            next_probe_ms: 0,
            last_ms: 0,
        })
    }
    pub fn clear(&mut self) {
        self.pending.clear();
        self.paths.clear();
        self.candidates.clear();
        self.probes.clear();
    }
    pub fn maintain(&mut self, coordinator: &mut Coordinator, now_ms: u64) -> Result<(), Error> {
        if now_ms < self.last_ms {
            self.clear();
            coordinator.disconnect();
            return Err(Error::Clock);
        }
        self.last_ms = now_ms;
        self.probes
            .retain(|peer, _| coordinator.route(*peer, now_ms).ok().flatten().is_some());
        self.candidates.retain(|peer, candidates| {
            candidates.deadline > now_ms
                && coordinator
                    .data_plane()
                    .validate_handshake(&candidates.permit, now_ms)
                    .is_ok()
                && coordinator
                    .route(*peer, now_ms)
                    .ok()
                    .flatten()
                    .is_some_and(|route| route.discovery_key == Some(candidates.disco))
        });
        let valid = |p: &Path, c: &mut Coordinator| {
            p.deadline > now_ms
                && c.data_plane().validate_handshake(&p.permit, now_ms).is_ok()
                && c.route(p.peer, now_ms).ok().flatten().is_some_and(|route| {
                    route.discovery_key == Some(p.disco)
                        && (route.endpoints.contains(&p.endpoint)
                            || self
                                .candidates
                                .get(&p.peer)
                                .is_some_and(|c| c.endpoints.contains(&p.endpoint)))
                })
        };
        self.pending.retain(|_, p| valid(p, coordinator));
        self.paths.retain(|_, p| valid(p, coordinator));
        Ok(())
    }
    /// Selects a currently validated direct path, otherwise a usable authorized
    /// home relay region. Call again before sending work delayed in a queue.
    pub fn select(
        &mut self,
        coordinator: &mut Coordinator,
        peer: Key,
        now_ms: u64,
    ) -> Result<Option<Route>, Error> {
        if let Some(endpoint) = self.endpoint(coordinator, peer, now_ms)? {
            return Ok(Some(Route::Direct(endpoint)));
        }
        let Some(region_id) = coordinator
            .route(peer, now_ms)?
            .and_then(|route| route.relay_region)
        else {
            return Ok(None);
        };
        let usable = coordinator
            .relay_for(peer, now_ms)?
            .is_some_and(|region| region.nodes.iter().any(|node| !node.stun_only));
        Ok(usable.then_some(Route::Relay(region_id)))
    }
    /// Report an underlay send/path failure before selecting the next route.
    /// This withdraws direct-path validation without revoking WireGuard keys.
    pub fn failed_endpoint(&mut self, peer: Key, endpoint: SocketAddr) {
        if self
            .paths
            .get(&peer)
            .is_some_and(|path| path.endpoint == endpoint)
        {
            self.paths.remove(&peer);
        }
    }
    /// Schedules at most one probe: globally 50 ms apart, per peer 500 ms apart.
    /// Validated paths are refreshed every ten seconds. No hidden send queue.
    pub fn next_probe(
        &mut self,
        coordinator: &mut Coordinator,
        peer: Key,
        now_ms: u64,
    ) -> Result<Option<Probe>, Error> {
        self.maintain(coordinator, now_ms)?;
        if now_ms < self.next_probe_ms
            || self.probes.get(&peer).is_some_and(|p| now_ms < p.next_ms)
            || self.pending.len() >= 32
        {
            return Ok(None);
        }
        let Some(route) = coordinator.route(peer, now_ms)? else {
            return Ok(None);
        };
        if route.discovery_key.is_none() {
            return Ok(None);
        }
        let validated = self.endpoint(coordinator, peer, now_ms)?;
        let endpoints = match validated {
            Some(endpoint) => vec![endpoint],
            None => self.candidates(coordinator, peer, now_ms)?,
        };
        if endpoints.is_empty() {
            return Ok(None);
        }
        if !self.probes.contains_key(&peer) && self.probes.len() >= 256 {
            return Err(Error::Capacity);
        }
        let start = self.probes.get(&peer).map_or(0, |p| p.index) % endpoints.len();
        for offset in 0..endpoints.len() {
            let index = (start + offset) % endpoints.len();
            let endpoint = endpoints[index];
            if self
                .pending
                .values()
                .any(|p| p.peer == peer && p.endpoint == endpoint)
            {
                continue;
            }
            let global_next = now_ms.checked_add(50).ok_or(Error::Clock)?;
            // A peer without a direct path is tried briskly at first, then
            // only often enough to notice when one appears.
            let unanswered = match validated {
                Some(_) => 0,
                None => self.probes.get(&peer).map_or(0, |p| p.unanswered) + 1,
            };
            let peer_next = now_ms
                .checked_add(match (validated, unanswered) {
                    (Some(_), _) => 10000,
                    (None, ..=BRISK_PROBES) => 500,
                    (None, _) => 5000,
                })
                .ok_or(Error::Clock)?;
            let packet = self.ping(coordinator, peer, endpoint, now_ms)?;
            self.probes.insert(
                peer,
                ProbeCursor {
                    index: (index + 1) % endpoints.len(),
                    next_ms: peer_next,
                    unanswered,
                },
            );
            self.next_probe_ms = global_next;
            return Ok(Some(Probe { endpoint, packet }));
        }
        Ok(None)
    }
    pub fn ping(
        &mut self,
        coordinator: &mut Coordinator,
        peer: Key,
        endpoint: SocketAddr,
        now_ms: u64,
    ) -> Result<Vec<u8>, Error> {
        self.maintain(coordinator, now_ms)?;
        valid_endpoint(endpoint)?;
        let route = coordinator.route(peer, now_ms)?.ok_or(Error::Unknown)?;
        let disco = route.discovery_key.ok_or(Error::Invalid)?;
        if !route.endpoints.contains(&endpoint)
            && !self
                .candidates
                .get(&peer)
                .is_some_and(|c| c.endpoints.contains(&endpoint))
        {
            return Err(Error::Invalid);
        }
        if coordinator.peer_for_discovery(disco, now_ms)? != Some(peer) {
            return Err(Error::Invalid);
        }
        if self.pending.len() >= 32
            || self
                .pending
                .values()
                .any(|p| p.peer == peer && p.endpoint == endpoint)
        {
            return Err(Error::Capacity);
        }
        let permit = coordinator.data_plane().begin_handshake(peer, now_ms)?;
        let mut id = [0; 12];
        getrandom::getrandom(&mut id).map_err(|_| Error::Random)?;
        if self.pending.contains_key(&id) {
            return Err(Error::Random);
        }
        let packet = disco::seal_ping(&self.local_disco, disco, id, self.local_node)?;
        self.pending.insert(
            id,
            Path {
                peer,
                disco,
                endpoint,
                permit,
                deadline: now_ms.checked_add(5000).ok_or(Error::Clock)?,
            },
        );
        Ok(packet)
    }
    /// Encodes local interface/STUN candidates for delivery over DERP. The
    /// caller supplies current local observations; no socket is opened here.
    pub fn call_me_maybe(
        &mut self,
        coordinator: &mut Coordinator,
        peer: Key,
        endpoints: &[SocketAddr],
        now_ms: u64,
    ) -> Result<Vec<u8>, Error> {
        self.maintain(coordinator, now_ms)?;
        let endpoints = checked_endpoints(endpoints)?;
        let disco = coordinator
            .route(peer, now_ms)?
            .and_then(|r| r.discovery_key)
            .ok_or(Error::Unknown)?;
        if coordinator.peer_for_discovery(disco, now_ms)? != Some(peer) {
            return Err(Error::Invalid);
        }
        Ok(disco::seal_call_me_maybe(
            &self.local_disco,
            disco,
            &endpoints,
        )?)
    }
    /// Only DERP adapters may call this entry point, using the relay's source
    /// node key. This never promotes an endpoint to a validated direct path.
    pub fn receive_call_me_maybe(
        &mut self,
        coordinator: &mut Coordinator,
        relay_peer: Key,
        packet: &[u8],
        now_ms: u64,
    ) -> Result<Key, Error> {
        self.maintain(coordinator, now_ms)?;
        if !(98..=368).contains(&packet.len()) {
            return Err(Error::Invalid);
        }
        let message = disco::open_call_me_maybe(&self.local_disco, packet)?;
        let peer = coordinator
            .peer_for_discovery(message.sender_disco_key, now_ms)?
            .ok_or(Error::Unknown)?;
        if peer != relay_peer {
            return Err(Error::Invalid);
        }
        let endpoints = checked_endpoints(&message.endpoints)?;
        if !self.candidates.contains_key(&peer) && self.candidates.len() >= 256 {
            return Err(Error::Capacity);
        }
        let permit = coordinator.data_plane().begin_handshake(peer, now_ms)?;
        self.candidates.insert(
            peer,
            Candidates {
                disco: message.sender_disco_key,
                endpoints,
                permit,
                deadline: now_ms.checked_add(30000).ok_or(Error::Clock)?,
            },
        );
        self.maintain(coordinator, now_ms)?;
        Ok(peer)
    }
    /// At most 32 unique endpoints from the coordinator and current DERP hints.
    /// The caller must probe them before selecting a direct path.
    pub fn candidates(
        &mut self,
        coordinator: &mut Coordinator,
        peer: Key,
        now_ms: u64,
    ) -> Result<Vec<SocketAddr>, Error> {
        self.maintain(coordinator, now_ms)?;
        let Some(route) = coordinator.route(peer, now_ms)? else {
            return Ok(Vec::new());
        };
        let mut endpoints = route.endpoints.clone();
        if let Some(candidates) = self.candidates.get(&peer) {
            for endpoint in &candidates.endpoints {
                if !endpoints.contains(endpoint) {
                    endpoints.push(*endpoint);
                }
            }
        }
        Ok(endpoints)
    }
    /// A ping may elicit a response but cannot establish a validated path.
    pub fn reply(
        &mut self,
        coordinator: &mut Coordinator,
        source: SocketAddr,
        packet: &[u8],
        now_ms: u64,
    ) -> Result<(Key, Vec<u8>), Error> {
        self.maintain(coordinator, now_ms)?;
        valid_endpoint(source)?;
        if !(92..=1536).contains(&packet.len()) {
            return Err(Error::Invalid);
        }
        let ping = disco::open_ping(&self.local_disco, packet)?;
        let peer = coordinator
            .peer_for_discovery(ping.sender_disco_key, now_ms)?
            .ok_or(Error::Unknown)?;
        if ping
            .node_key
            .is_some_and(|node| node != Key::zero() && node != peer)
        {
            return Err(Error::Invalid);
        }
        Ok((
            peer,
            disco::seal_pong(
                &self.local_disco,
                ping.sender_disco_key,
                ping.transaction_id,
                source,
            )?,
        ))
    }
    pub fn pong(
        &mut self,
        coordinator: &mut Coordinator,
        source: SocketAddr,
        packet: &[u8],
        now_ms: u64,
    ) -> Result<Key, Error> {
        self.maintain(coordinator, now_ms)?;
        if !(110..=1536).contains(&packet.len()) {
            return Err(Error::Invalid);
        }
        let pong = disco::open_pong(&self.local_disco, packet)?;
        let pending = self
            .pending
            .get(&pong.transaction_id)
            .ok_or(Error::Unknown)?;
        if pending.disco != pong.sender_disco_key || pending.endpoint != source {
            return Err(Error::Invalid);
        }
        valid_endpoint(pong.observed_source)?;
        let deadline = now_ms.checked_add(30000).ok_or(Error::Clock)?;
        let mut path = self
            .pending
            .remove(&pong.transaction_id)
            .ok_or(Error::Unknown)?;
        path.deadline = deadline;
        let peer = path.peer;
        if !self.paths.contains_key(&peer) && self.paths.len() >= 256 {
            return Err(Error::Capacity);
        }
        self.paths.insert(peer, path);
        Ok(peer)
    }
    pub fn endpoint(
        &mut self,
        coordinator: &mut Coordinator,
        peer: Key,
        now_ms: u64,
    ) -> Result<Option<SocketAddr>, Error> {
        self.maintain(coordinator, now_ms)?;
        Ok(self.paths.get(&peer).map(|p| p.endpoint))
    }
}
fn valid_endpoint(endpoint: SocketAddr) -> Result<(), Error> {
    let ip = endpoint.ip().to_canonical();
    if endpoint.port() == 0
        || ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || matches!(ip,std::net::IpAddr::V4(ip) if ip.is_broadcast())
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn checked_endpoints(endpoints: &[SocketAddr]) -> Result<Vec<SocketAddr>, Error> {
    if endpoints.is_empty() || endpoints.len() > 16 {
        return Err(Error::Capacity);
    }
    let mut checked = Vec::with_capacity(endpoints.len());
    for endpoint in endpoints {
        valid_endpoint(*endpoint)?;
        let endpoint = SocketAddr::new(endpoint.ip().to_canonical(), endpoint.port());
        if checked.contains(&endpoint) {
            return Err(Error::Invalid);
        }
        checked.push(endpoint);
    }
    Ok(checked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{coordinator::Clock, protocol::identity::DeviceIdentity};
    use serde_json::json;
    fn clock(n: u64) -> Clock {
        Clock {
            unix_ms: 1_735_689_600_000 + n as i64,
            monotonic_ms: n,
        }
    }
    fn coordinator(
        local: &DeviceIdentity,
        remote: &DeviceIdentity,
        endpoint: SocketAddr,
    ) -> Coordinator {
        let mut c = Coordinator::discovering(local.node_key().public(), 4, 2, 0).unwrap();
        let map = json!({"Node":{"ID":1,"Key":local.node_key().public().to_string(),"Addresses":["100.64.0.1/32"],"MachineAuthorized":true},"Peers":[{"ID":2,"Key":remote.node_key().public().to_string(),"DiscoKey":remote.disco_key().public().to_string(),"Addresses":["100.64.0.2/32"],"Endpoints":[endpoint.to_string()],"MachineAuthorized":true}],"PacketFilter":[]});
        c.apply(&serde_json::to_vec(&map).unwrap(), clock(0))
            .unwrap();
        c
    }
    #[test]
    fn authenticated_roundtrip_source_matching_replay_and_expiry() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:5678".parse().unwrap();
        let mut ca = coordinator(&a, &b, eb);
        let mut cb = coordinator(&b, &a, ea);
        let mut da = Discovery::new(a.node_key().public(), a.disco_key().clone()).unwrap();
        let mut db = Discovery::new(b.node_key().public(), b.disco_key().clone()).unwrap();
        let ping = da.ping(&mut ca, b.node_key().public(), eb, 0).unwrap();
        let (peer, pong) = db.reply(&mut cb, ea, &ping, 1).unwrap();
        assert_eq!(peer, a.node_key().public());
        assert_eq!(
            db.endpoint(&mut cb, a.node_key().public(), 1).unwrap(),
            None
        );
        assert!(da.pong(&mut ca, ea, &pong, 2).is_err());
        let mut corrupt = pong.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(da.pong(&mut ca, eb, &corrupt, 2).is_err());
        assert_eq!(
            da.pong(&mut ca, eb, &pong, 2).unwrap(),
            b.node_key().public()
        );
        assert!(da.pong(&mut ca, eb, &pong, 2).is_err());
        assert_eq!(
            da.endpoint(&mut ca, b.node_key().public(), 3).unwrap(),
            Some(eb)
        );
        assert_eq!(
            da.endpoint(&mut ca, b.node_key().public(), 30002).unwrap(),
            None
        );
    }
    #[test]
    fn unanswered_probes_slow_down_after_a_brisk_start() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let peer = b.node_key().public();
        let mut c = Coordinator::discovering(a.node_key().public(), 4, 2, 0).unwrap();
        let map = json!({"Node":{"ID":1,"Key":a.node_key().public().to_string(),"Addresses":["100.64.0.1/32"],"MachineAuthorized":true},"Peers":[{"ID":2,"Key":peer.to_string(),"DiscoKey":b.disco_key().public().to_string(),"Addresses":["100.64.0.2/32"],"Endpoints":["192.0.2.2:1","192.0.2.2:2","192.0.2.2:3","192.0.2.2:4","192.0.2.2:5","192.0.2.2:6","192.0.2.2:7","192.0.2.2:8","192.0.2.2:9","192.0.2.2:10","192.0.2.2:11","192.0.2.2:12"],"MachineAuthorized":true}],"PacketFilter":[]});
        c.apply(&serde_json::to_vec(&map).unwrap(), clock(0))
            .unwrap();
        let mut d = Discovery::new(a.node_key().public(), a.disco_key().clone()).unwrap();
        let mut sent = Vec::new();
        for now in (0..40_000).step_by(100) {
            if d.next_probe(&mut c, peer, now).unwrap().is_some() {
                sent.push(now);
            }
        }
        let gaps: Vec<u64> = sent.windows(2).map(|pair| pair[1] - pair[0]).collect();
        let brisk = BRISK_PROBES as usize;
        assert!(gaps[..brisk].iter().all(|gap| *gap == 500));
        assert!(gaps[brisk..].iter().all(|gap| *gap == 5000));
        assert!(gaps.len() > brisk + 3);
    }
    #[test]
    fn map_updates_and_wrong_node_bindings_cannot_validate_paths() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:5678".parse().unwrap();
        let mut ca = coordinator(&a, &b, eb);
        let mut cb = coordinator(&b, &a, ea);
        let mut da = Discovery::new(a.node_key().public(), a.disco_key().clone()).unwrap();
        let mut db = Discovery::new(b.node_key().public(), b.disco_key().clone()).unwrap();
        let wrong = disco::seal_ping(
            a.disco_key(),
            b.disco_key().public(),
            [1; 12],
            b.node_key().public(),
        )
        .unwrap();
        assert!(db.reply(&mut cb, ea, &wrong, 0).is_err());
        let legacy =
            disco::seal_ping(a.disco_key(), b.disco_key().public(), [2; 12], Key::zero()).unwrap();
        assert!(db.reply(&mut cb, ea, &legacy, 0).is_ok());
        assert!(da.ping(&mut ca, b.node_key().public(), ea, 0).is_err());
        let ping = da.ping(&mut ca, b.node_key().public(), eb, 0).unwrap();
        let (_, pong) = db.reply(&mut cb, ea, &ping, 0).unwrap();
        ca.apply(br#"{"PeersRemoved":[2]}"#, clock(1)).unwrap();
        assert!(da.pong(&mut ca, eb, &pong, 1).is_err());
        assert_eq!(
            da.endpoint(&mut ca, b.node_key().public(), 1).unwrap(),
            None
        );
        assert!(da.maintain(&mut ca, 0).is_err());
    }
    #[test]
    fn ambiguous_discovery_keys_are_rejected() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let other = DeviceIdentity::generate().unwrap();
        let endpoint = "192.0.2.2:5678".parse().unwrap();
        let mut c = coordinator(&a, &b, endpoint);
        let map = json!({"PeersChanged":[{"ID":3,"Key":other.node_key().public().to_string(),"DiscoKey":b.disco_key().public().to_string(),"Addresses":["100.64.0.3/32"],"MachineAuthorized":true}]});
        c.apply(&serde_json::to_vec(&map).unwrap(), clock(1))
            .unwrap();
        assert!(c.peer_for_discovery(b.disco_key().public(), 1).is_err());
        let mut d = Discovery::new(a.node_key().public(), a.disco_key().clone()).unwrap();
        assert!(d.ping(&mut c, b.node_key().public(), endpoint, 1).is_err());
    }
    #[test]
    fn relay_candidates_require_probe_and_follow_replacement_and_revocation() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let old = "192.0.2.2:5678".parse().unwrap();
        let fresh = "192.0.2.2:6789".parse().unwrap();
        let mut ca = coordinator(&a, &b, old);
        let mut cb = coordinator(&b, &a, ea);
        let mut da = Discovery::new(ak, a.disco_key().clone()).unwrap();
        let mut db = Discovery::new(bk, b.disco_key().clone()).unwrap();
        assert!(da.ping(&mut ca, bk, fresh, 0).is_err());
        let message = db.call_me_maybe(&mut cb, ak, &[fresh], 0).unwrap();
        assert!(da.receive_call_me_maybe(&mut ca, ak, &message, 0).is_err());
        assert_eq!(
            da.receive_call_me_maybe(&mut ca, bk, &message, 0).unwrap(),
            bk
        );
        assert_eq!(da.candidates(&mut ca, bk, 0).unwrap(), vec![old, fresh]);
        assert_eq!(da.endpoint(&mut ca, bk, 0).unwrap(), None);
        let ping = da.ping(&mut ca, bk, fresh, 1).unwrap();
        let (_, pong) = db.reply(&mut cb, ea, &ping, 1).unwrap();
        da.pong(&mut ca, fresh, &pong, 2).unwrap();
        assert_eq!(da.endpoint(&mut ca, bk, 2).unwrap(), Some(fresh));
        let message = db.call_me_maybe(&mut cb, ak, &[old], 3).unwrap();
        da.receive_call_me_maybe(&mut ca, bk, &message, 3).unwrap();
        assert_eq!(da.endpoint(&mut ca, bk, 3).unwrap(), None);
        assert_eq!(da.candidates(&mut ca, bk, 3).unwrap(), vec![old]);
        ca.apply(br#"{"PeersRemoved":[2]}"#, clock(4)).unwrap();
        assert!(da.candidates(&mut ca, bk, 4).unwrap().is_empty());
        assert!(da.receive_call_me_maybe(&mut ca, bk, &message, 4).is_err());
    }
    #[test]
    fn relay_candidate_bounds_invalid_inputs_and_expiry() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let old = "192.0.2.2:5678".parse().unwrap();
        let fresh = "192.0.2.2:6789".parse().unwrap();
        let bk = b.node_key().public();
        let mut c = coordinator(&a, &b, old);
        let mut d = Discovery::new(a.node_key().public(), a.disco_key().clone()).unwrap();
        let good =
            disco::seal_call_me_maybe(b.disco_key(), a.disco_key().public(), &[fresh]).unwrap();
        d.receive_call_me_maybe(&mut c, bk, &good, 0).unwrap();
        for endpoints in [
            vec![fresh; 17],
            vec![fresh, fresh],
            vec!["127.0.0.1:1234".parse().unwrap()],
            vec!["0.0.0.0:1234".parse().unwrap()],
        ] {
            let message =
                disco::seal_call_me_maybe(b.disco_key(), a.disco_key().public(), &endpoints)
                    .unwrap();
            assert!(d.receive_call_me_maybe(&mut c, bk, &message, 1).is_err());
        }
        let mut tampered = good.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(d.receive_call_me_maybe(&mut c, bk, &tampered, 1).is_err());
        assert_eq!(d.candidates(&mut c, bk, 1).unwrap(), vec![old, fresh]);
        assert_eq!(d.candidates(&mut c, bk, 30000).unwrap(), vec![old]);
        assert!(d.ping(&mut c, bk, fresh, 30000).is_err());
    }
    #[test]
    fn selection_falls_back_to_usable_relay_and_recovers_direct() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:5678".parse().unwrap();
        let mut ca = coordinator(&a, &b, eb);
        let mut cb = coordinator(&b, &a, ea);
        let map = json!({"PeersChangedPatch":[{"NodeID":2,"DERPRegion":7}],"DERPMap":{"Regions":{"7":{"RegionID":7,"Nodes":[{"Name":"7a","RegionID":7,"HostName":"relay.example"}]}}}});
        ca.apply(&serde_json::to_vec(&map).unwrap(), clock(0))
            .unwrap();
        let mut da = Discovery::new(ak, a.disco_key().clone()).unwrap();
        let mut db = Discovery::new(bk, b.disco_key().clone()).unwrap();
        assert_eq!(da.select(&mut ca, bk, 0).unwrap(), Some(Route::Relay(7)));
        let probe = da.next_probe(&mut ca, bk, 0).unwrap().unwrap();
        let (_, pong) = db.reply(&mut cb, ea, &probe.packet, 1).unwrap();
        da.pong(&mut ca, eb, &pong, 2).unwrap();
        assert_eq!(da.select(&mut ca, bk, 2).unwrap(), Some(Route::Direct(eb)));
        assert!(da.next_probe(&mut ca, bk, 499).unwrap().is_none());
        assert!(da.next_probe(&mut ca, bk, 500).unwrap().is_some());
        assert!(da.next_probe(&mut ca, bk, 10499).unwrap().is_none());
        da.failed_endpoint(bk, eb);
        assert_eq!(
            da.select(&mut ca, bk, 10499).unwrap(),
            Some(Route::Relay(7))
        );
        ca.apply(br#"{"DERPMap":{"Regions":{"7":{"RegionID":7,"Nodes":[{"Name":"7a","RegionID":7,"HostName":"relay.example","STUNOnly":true}]}}}}"#,clock(10500)).unwrap();
        assert_eq!(da.select(&mut ca, bk, 10500).unwrap(), None);
        ca.apply(br#"{"DERPMap":{"Regions":{}}}"#, clock(10501))
            .unwrap();
        assert_eq!(da.select(&mut ca, bk, 10501).unwrap(), None);
        ca.disconnect();
        assert!(da.next_probe(&mut ca, bk, 10501).unwrap().is_none());
        assert_eq!(da.select(&mut ca, bk, 10501).unwrap(), None);
    }
    #[test]
    fn probes_rotate_candidates_with_bounded_cadence() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let c = DeviceIdentity::generate().unwrap();
        let bk = b.node_key().public();
        let ck = c.node_key().public();
        let first = "192.0.2.2:5678".parse().unwrap();
        let second: SocketAddr = "192.0.2.2:6789".parse().unwrap();
        let mut coordinator = coordinator(&a, &b, first);
        let map = json!({"PeersChangedPatch":[{"NodeID":2,"Endpoints":[first.to_string(),second.to_string()]}],"PeersChanged":[{"ID":3,"Key":ck.to_string(),"DiscoKey":c.disco_key().public().to_string(),"Addresses":["100.64.0.3/32"],"Endpoints":["192.0.2.3:1234"],"MachineAuthorized":true}]});
        coordinator
            .apply(&serde_json::to_vec(&map).unwrap(), clock(0))
            .unwrap();
        let mut d = Discovery::new(a.node_key().public(), a.disco_key().clone()).unwrap();
        assert_eq!(
            d.next_probe(&mut coordinator, bk, 0)
                .unwrap()
                .unwrap()
                .endpoint,
            first
        );
        assert!(d.next_probe(&mut coordinator, ck, 49).unwrap().is_none());
        assert!(d.next_probe(&mut coordinator, ck, 50).unwrap().is_some());
        assert!(d.next_probe(&mut coordinator, bk, 499).unwrap().is_none());
        assert_eq!(
            d.next_probe(&mut coordinator, bk, 500)
                .unwrap()
                .unwrap()
                .endpoint,
            second
        );
        assert!(d.next_probe(&mut coordinator, bk, 1000).unwrap().is_none());
        assert_eq!(
            d.next_probe(&mut coordinator, bk, 5000)
                .unwrap()
                .unwrap()
                .endpoint,
            first
        );
        assert_eq!(
            d.next_probe(&mut coordinator, bk, 5500)
                .unwrap()
                .unwrap()
                .endpoint,
            second
        );
    }
}
