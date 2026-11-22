//! Single-owner control, discovery, handshake, and bounded packet queues.
use crate::{
    coordinator::{Clock, Coordinator, MapError},
    discovery::{Discovery, Route},
    handshake::{Event, Handshakes},
    plane::{HandshakePermit, PendingPacket, PlaneError},
    protocol::{
        identity::DeviceIdentity,
        key::{Node, PublicKey},
    },
    timestamp::{EpochStore, TimestampClock},
};
use std::{
    collections::{BTreeMap, VecDeque},
    io,
    net::{Ipv4Addr, SocketAddr},
};
#[path = "network_tcp.rs"]
mod tcp_owner;
type Key = PublicKey<Node>;
const OUTGOING: usize = 16;
const INCOMING: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network queue capacity reached")]
    Capacity,
    #[error("invalid network datagram or relay source")]
    Packet,
    #[error("network clock moved backwards or overflowed")]
    Clock,
    #[error(transparent)]
    Map(#[from] MapError),
    #[error(transparent)]
    Plane(#[from] PlaneError),
    #[error(transparent)]
    Handshake(#[from] crate::handshake::Error),
    #[error(transparent)]
    Discovery(#[from] crate::discovery::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Stun(#[from] crate::stun::Error),
    #[error(transparent)]
    Udp(#[from] crate::udp::Error),
    #[error(transparent)]
    Tcp(#[from] crate::tcp::Error),
}
#[derive(Clone, Copy)]
pub enum Source {
    Udp(SocketAddr),
    Relay(Key),
}
#[derive(Clone, Copy)]
enum Destination {
    Selected,
    UdpReply(SocketAddr),
    Probe(SocketAddr),
    Relay,
}
struct Outbound {
    keepalive_reply: bool,
    peer: Key,
    destination: Destination,
    bytes: Vec<u8>,
    permit: HandshakePermit,
    session: Option<std::sync::Arc<()>>,
    deadline: u64,
    udp_socket: Option<crate::udp::Socket>,
    tcp_socket: Option<crate::tcp::Socket>,
}
struct Keepalive {
    deadline: u64,
    again: bool,
    queued: bool,
    authorization: std::sync::Arc<()>,
    session: std::sync::Arc<()>,
}
struct RetrySeries {
    expires: u64,
    authorization: std::sync::Arc<()>,
    session: Option<std::sync::Arc<()>>,
}
struct Silence {
    deadline: u64,
    authorization: std::sync::Arc<()>,
    session: std::sync::Arc<()>,
}
impl Outbound {
    fn authenticated(&self) -> bool {
        self.session.is_some()
            || self.bytes.starts_with(&1u32.to_le_bytes())
            || self.bytes.starts_with(&2u32.to_le_bytes())
    }
}

/// Opaque authority for a queued datagram, including session identity and expiry.
#[derive(Clone)]
pub struct DeliveryPermit {
    peer: Key,
    permit: HandshakePermit,
    session: Option<std::sync::Arc<()>>,
    deadline: u64,
    udp_socket: Option<crate::udp::Socket>,
    tcp_socket: Option<crate::tcp::Socket>,
}
/// Immutable relay selection bound to effective authorization and its owner.
#[derive(Clone)]
pub struct RelayPlan {
    region: u64,
    home: bool,
    node: crate::relay::RelayNode,
    local_node: Key,
    authorization: std::sync::Arc<()>,
    directory: std::sync::Arc<()>,
}
impl RelayPlan {
    pub fn is_home(&self) -> bool {
        self.home
    }
    pub fn region(&self) -> u64 {
        self.region
    }
    pub fn node(&self) -> &crate::relay::RelayNode {
        &self.node
    }
    pub fn local_node(&self) -> Key {
        self.local_node
    }
}

/// Content-free counters for one network owner. Counters saturate and survive
/// disconnects; a new owner starts at zero. Receive acceptance is not application
/// delivery. Send acceptance means the adapter accepted the packet, not delivery.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Diagnostics {
    pub receive_calls: u64,
    pub receive_rejected: u64,
    pub send_accepted: u64,
    pub send_would_block: u64,
    pub send_errors: u64,
    pub outgoing_packets: usize,
    pub outgoing_capacity_bytes: usize,
    pub incoming_packets: usize,
    pub incoming_capacity_bytes: usize,
    pub tcp_connections: usize,
    pub known_peers: usize,
    pub usable_peers: usize,
}

pub struct Network<S: EpochStore> {
    diagnostics: Diagnostics,
    local_node: Key,
    coordinator: Coordinator,
    discovery: Discovery,
    handshakes: Handshakes,
    timestamps: TimestampClock<S>,
    outgoing: VecDeque<Outbound>,
    incoming: VecDeque<(Key, PendingPacket, Option<u16>)>,
    udp_bindings: crate::udp::Bindings,
    tcp_connections: BTreeMap<u64, tcp_owner::Connection>,
    tcp_next: u64,
    tcp_port: u32,
    last_ms: u64,
    probe_cursor: Option<u64>,
    next_probe_scan_ms: u64,
    endpoints: crate::stun::EndpointDiscovery,
    /// The relay region each STUN server measures.
    stun_regions: Vec<(u64, std::net::SocketAddrV4)>,
    handshake_retry: BTreeMap<Key, u64>,
    retry_series: BTreeMap<Key, RetrySeries>,
    keepalives: BTreeMap<Key, Keepalive>,
    silence: BTreeMap<Key, Silence>,
    next_handshake_ms: u64,
}
impl<S: EpochStore> Network<S> {
    /// The timestamp store must belong to this identity. Transport adapters and
    /// authenticated map-stream ingestion remain outside this portable owner.
    pub fn new(
        identity: &DeviceIdentity,
        timestamps: TimestampClock<S>,
        max_peers: usize,
        max_sessions: usize,
    ) -> Result<Self, Error> {
        Self::with_address(identity, timestamps, max_peers, max_sessions, None)
    }
    /// An explicit address is checked against authenticated local-node maps.
    /// Without one, the initial map supplies the assigned IPv4 address.
    pub fn with_address(
        identity: &DeviceIdentity,
        timestamps: TimestampClock<S>,
        max_peers: usize,
        max_sessions: usize,
        address: Option<Ipv4Addr>,
    ) -> Result<Self, Error> {
        Ok(Self {
            diagnostics: Diagnostics::default(),
            local_node: identity.node_key().public(),
            coordinator: match address {
                Some(address) => Coordinator::new(
                    identity.node_key().public(),
                    address,
                    max_peers,
                    max_sessions,
                    0,
                )?,
                None => Coordinator::discovering(
                    identity.node_key().public(),
                    max_peers,
                    max_sessions,
                    0,
                )?,
            },
            discovery: Discovery::new(identity.node_key().public(), identity.disco_key().clone())?,
            handshakes: Handshakes::new(identity.node_key().clone(), max_sessions, 0)?,
            timestamps,
            outgoing: VecDeque::new(),
            incoming: VecDeque::new(),
            udp_bindings: Default::default(),
            tcp_connections: BTreeMap::new(),
            tcp_next: 0,
            tcp_port: 49152,
            last_ms: 0,
            probe_cursor: None,
            next_probe_scan_ms: 0,
            endpoints: Default::default(),
            stun_regions: Vec::new(),
            handshake_retry: BTreeMap::new(),
            retry_series: BTreeMap::new(),
            keepalives: BTreeMap::new(),
            silence: BTreeMap::new(),
            next_handshake_ms: 0,
        })
    }
    /// Allocation-free snapshot. Buffer capacities exclude container metadata,
    /// allocator overhead, protocol state and transport-adapter queues.
    pub fn diagnostics(&self) -> Diagnostics {
        let (known_peers, usable_peers) = self.coordinator.peer_counts();
        Diagnostics {
            known_peers,
            usable_peers,
            outgoing_packets: self.outgoing.len(),
            outgoing_capacity_bytes: self.outgoing.iter().map(|p| p.bytes.capacity()).sum(),
            incoming_packets: self.incoming.len(),
            incoming_capacity_bytes: self.incoming.iter().map(|(_, p, _)| p.capacity()).sum(),
            tcp_connections: self.tcp_connections.len(),
            ..self.diagnostics
        }
    }
    /// Authenticated session installations, including peer-initiated renewals.
    pub fn completed_handshakes(&self) -> crate::handshake::Completed {
        self.handshakes.completed()
    }
    pub fn disconnect(&mut self) {
        self.tcp_connections.clear();
        self.handshake_retry.clear();
        self.retry_series.clear();
        self.keepalives.clear();
        self.silence.clear();
        self.udp_bindings.clear();
        self.endpoints.clear();
        self.probe_cursor = None;
        self.next_probe_scan_ms = 0;
        self.coordinator.disconnect();
        self.discovery.clear();
        self.handshakes.clear();
        let _ = self
            .handshakes
            .maintain(self.coordinator.data_plane(), self.last_ms);
        self.outgoing.clear();
        self.incoming.clear();
    }
    pub fn tick(&mut self, now_ms: u64) -> Result<(), Error> {
        if now_ms < self.last_ms {
            self.disconnect();
            return Err(Error::Clock);
        }
        self.last_ms = now_ms;
        if self.coordinator.local_address(now_ms)?.is_none() {
            self.tcp_connections.clear();
            self.udp_bindings.clear();
            self.endpoints.clear();
            self.outgoing.clear();
            self.incoming.clear();
        }
        self.coordinator.data_plane().maintain(now_ms)?;
        self.handshakes
            .maintain(self.coordinator.data_plane(), now_ms)?;
        self.discovery.maintain(&mut self.coordinator, now_ms)?;
        self.endpoints.maintain(now_ms)?;
        self.handshake_retry.retain(|peer, deadline| {
            *deadline > now_ms
                && self
                    .coordinator
                    .data_plane()
                    .begin_handshake(*peer, now_ms)
                    .is_ok()
        });
        self.tcp_connections.retain(|_, connection| {
            self.coordinator
                .data_plane()
                .validate_handshake(&connection.permit, now_ms)
                .is_ok()
        });
        self.outgoing.retain(|packet| {
            (!packet.keepalive_reply || self.keepalives.contains_key(&packet.peer))
                && packet
                    .tcp_socket
                    .as_ref()
                    .is_none_or(|socket| tcp_owner::matches(&self.tcp_connections, socket))
                && packet.deadline > now_ms
                && self
                    .coordinator
                    .data_plane()
                    .validate_handshake(&packet.permit, now_ms)
                    .is_ok()
                && packet.session.as_ref().is_none_or(|token| {
                    self.coordinator
                        .data_plane()
                        .session_identity(packet.peer, now_ms)
                        .ok()
                        .flatten()
                        .is_some_and(|current| std::sync::Arc::ptr_eq(token, &current))
                })
        });
        self.poll_keepalives(now_ms)?;
        self.poll_silence(now_ms);
        self.poll_handshake_retries(now_ms);
        Ok(())
    }
    fn poll_handshake_retries(&mut self, now_ms: u64) {
        self.retry_series.retain(|peer, series| {
            if now_ms >= series.expires
                || !std::sync::Arc::ptr_eq(
                    &series.authorization,
                    &self.coordinator.data_plane().authorization_identity(),
                )
                || self
                    .coordinator
                    .data_plane()
                    .begin_handshake(*peer, now_ms)
                    .is_err()
            {
                return false;
            }
            match self
                .coordinator
                .data_plane()
                .session_identity(*peer, now_ms)
            {
                Ok(Some(current)) => series
                    .session
                    .as_ref()
                    .is_some_and(|old| std::sync::Arc::ptr_eq(old, &current)),
                Ok(None) => true,
                Err(_) => false,
            }
        });
        let peer = self
            .retry_series
            .keys()
            .find(|peer| {
                !self.handshake_retry.contains_key(peer) && !self.handshakes.pending_for(**peer)
            })
            .copied();
        if let Some(peer) = peer {
            let _ = self.schedule_handshake(peer, now_ms);
        }
    }
    fn poll_silence(&mut self, now_ms: u64) {
        self.silence.retain(|peer, timer| {
            std::sync::Arc::ptr_eq(
                &timer.authorization,
                &self.coordinator.data_plane().authorization_identity(),
            ) && self
                .coordinator
                .data_plane()
                .session_identity(*peer, now_ms)
                .ok()
                .flatten()
                .is_some_and(|session| std::sync::Arc::ptr_eq(&session, &timer.session))
        });
        let peer = self.silence.iter().find_map(|(peer, timer)| {
            (timer.deadline <= now_ms
                && !self.handshakes.pending_for(*peer)
                && !self.handshake_retry.contains_key(peer))
            .then_some(*peer)
        });
        if let Some(peer) = peer
            && matches!(self.schedule_handshake(peer, now_ms), Ok(true))
        {
            self.silence.remove(&peer);
        }
    }
    fn arm_silence(&mut self, packet: &Outbound, now_ms: u64) -> Result<(), Error> {
        if self.silence.contains_key(&packet.peer) {
            return Ok(());
        }
        if self.silence.len() >= 256 {
            return Err(Error::Capacity);
        }
        let mut random = [0; 4];
        getrandom::getrandom(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
        let deadline = now_ms
            .checked_add(15_000 + u64::from(u32::from_le_bytes(random) % 334))
            .ok_or(Error::Clock)?;
        let session = packet.session.clone().ok_or(Error::Packet)?;
        self.silence.insert(
            packet.peer,
            Silence {
                deadline,
                authorization: self.coordinator.data_plane().authorization_identity(),
                session,
            },
        );
        Ok(())
    }
    fn poll_keepalives(&mut self, now_ms: u64) -> Result<(), Error> {
        self.keepalives.retain(|peer, timer| {
            std::sync::Arc::ptr_eq(
                &timer.authorization,
                &self.coordinator.data_plane().authorization_identity(),
            ) && self
                .coordinator
                .data_plane()
                .session_identity(*peer, now_ms)
                .ok()
                .flatten()
                .is_some_and(|session| std::sync::Arc::ptr_eq(&session, &timer.session))
        });
        for (peer, timer) in &mut self.keepalives {
            if timer.queued
                && !self
                    .outgoing
                    .iter()
                    .any(|p| p.peer == *peer && p.keepalive_reply)
            {
                timer.queued = false;
            }
        }
        if self.outgoing.len() >= OUTGOING {
            return Ok(());
        }
        let peer = self.keepalives.iter().find_map(|(peer, timer)| {
            (timer.deadline <= now_ms
                && !timer.queued
                && !self
                    .outgoing
                    .iter()
                    .any(|p| p.peer == *peer && p.authenticated()))
            .then_some(*peer)
        });
        if let Some(peer) = peer {
            let bytes = self.coordinator.data_plane().send(peer, &[], now_ms)?;
            self.queue(peer, Destination::Selected, bytes, now_ms)?;
            self.outgoing
                .back_mut()
                .expect("queued keepalive")
                .keepalive_reply = true;
            self.keepalives
                .get_mut(&peer)
                .expect("keepalive timer")
                .queued = true;
        }
        Ok(())
    }
    fn arm_keepalive(&mut self, peer: Key, now_ms: u64) -> Result<(), Error> {
        let session = self
            .coordinator
            .data_plane()
            .session_identity(peer, now_ms)?
            .ok_or(Error::Packet)?;
        if let Some(timer) = self.keepalives.get_mut(&peer)
            && std::sync::Arc::ptr_eq(&timer.session, &session)
        {
            timer.again = true;
            return Ok(());
        }
        self.keepalives.remove(&peer);
        if self.keepalives.len() >= 256 {
            return Err(Error::Capacity);
        }
        let deadline = now_ms.checked_add(10_000).ok_or(Error::Clock)?;
        let authorization = self.coordinator.data_plane().authorization_identity();
        self.keepalives.insert(
            peer,
            Keepalive {
                deadline,
                again: false,
                queued: false,
                authorization,
                session,
            },
        );
        Ok(())
    }
    /// Only already authenticated map bodies may enter here. Queue invalidation
    /// and protocol-state maintenance occur in the same exclusive operation.
    pub fn apply_map(&mut self, body: &[u8], clock: Clock) -> Result<(), Error> {
        self.tick(clock.monotonic_ms)?;
        let authorization = self.coordinator.data_plane().authorization_identity();
        match self.coordinator.apply_event(body, clock) {
            Ok(false) => return self.tick(clock.monotonic_ms),
            Ok(true) => {}
            Err(error) => {
                self.disconnect();
                return Err(error.into());
            }
        }
        if std::sync::Arc::ptr_eq(
            &authorization,
            &self.coordinator.data_plane().authorization_identity(),
        ) && let Err(error) = tcp_owner::refresh_authorization(self, clock.monotonic_ms)
        {
            self.disconnect();
            return Err(error);
        }
        self.outgoing.clear();
        self.incoming.clear();
        self.probe_cursor = None;
        self.stun_regions = self.coordinator.stun_servers(clock.monotonic_ms)?;
        let servers: Vec<_> = self
            .stun_regions
            .iter()
            .map(|(_, server)| *server)
            .collect();
        if let Err(error) = self.endpoints.configure(&servers) {
            self.disconnect();
            return Err(error.into());
        }
        self.tick(clock.monotonic_ms)
    }
    /// Sends at most one scheduled STUN request through the peer UDP socket.
    pub fn send_stun(
        &mut self,
        now_ms: u64,
        mut send: impl FnMut(SocketAddr, &[u8]) -> io::Result<()>,
    ) -> Result<bool, Error> {
        self.tick(now_ms)?;
        let Some((server, packet)) = self.endpoints.request(now_ms)? else {
            return Ok(false);
        };
        send(SocketAddr::V4(server), &packet)?;
        Ok(true)
    }
    /// Current unauthenticated public-endpoint hints; never a peer identity.
    pub fn observed_endpoints(
        &mut self,
        now_ms: u64,
    ) -> Result<Vec<std::net::SocketAddrV4>, Error> {
        self.tick(now_ms)?;
        Ok(self.endpoints.endpoints(now_ms)?)
    }
    pub fn local_address(&mut self, now_ms: u64) -> Result<Option<Ipv4Addr>, Error> {
        self.tick(now_ms)?;
        Ok(self.coordinator.local_address(now_ms)?)
    }
    pub fn resolve(&mut self, name: &str, now_ms: u64) -> Result<Option<(Key, Ipv4Addr)>, Error> {
        self.tick(now_ms)?;
        Ok(self.coordinator.resolve(name, now_ms)?)
    }
    pub fn relay_plan(&mut self, peer: Key, now_ms: u64) -> Result<Option<RelayPlan>, Error> {
        self.tick(now_ms)?;
        let Some(region) = self
            .coordinator
            .route(peer, now_ms)?
            .and_then(|route| route.relay_region)
        else {
            return Ok(None);
        };
        let Some(node) = self
            .coordinator
            .relay_for(peer, now_ms)?
            .and_then(|region| region.nodes.iter().find(|node| !node.stun_only))
            .cloned()
        else {
            return Ok(None);
        };
        Ok(Some(RelayPlan {
            region,
            home: false,
            node,
            local_node: self.local_node,
            authorization: self.coordinator.data_plane().authorization_identity(),
            directory: self.coordinator.relay_identity(),
        }))
    }
    /// The home relay to set up: the region whose STUN server answered
    /// fastest, or the first region when none answered. `None` until the
    /// first measurements are in.
    pub fn home_relay_plan(&mut self, now_ms: u64) -> Result<Option<RelayPlan>, Error> {
        self.home_relay_matching(now_ms, |_| true)
    }
    pub(crate) fn home_relay_matching(
        &mut self,
        now_ms: u64,
        mut accept: impl FnMut(&RelayPlan) -> bool,
    ) -> Result<Option<RelayPlan>, Error> {
        self.tick(now_ms)?;
        // The home relay is the nearest one; wait for the first measurements.
        if !self.endpoints.settled(now_ms) {
            return Ok(None);
        }
        let mut nearest: Vec<(u64, u64)> = self
            .endpoints
            .latencies()
            .filter_map(|(server, round_trip)| {
                let (region, _) = self
                    .stun_regions
                    .iter()
                    .find(|(_, other)| *other == server)?;
                Some((round_trip, *region))
            })
            .collect();
        nearest.sort_unstable();
        let nearest: Vec<u64> = nearest.into_iter().map(|(_, region)| region).collect();
        let authorization = self.coordinator.data_plane().authorization_identity();
        let local_node = self.local_node;
        let directory = self.coordinator.relay_identity();
        Ok(self
            .coordinator
            .home_relay_matching(now_ms, &nearest, |region, node| {
                accept(&RelayPlan {
                    region,
                    home: true,
                    node: node.clone(),
                    local_node,
                    authorization: authorization.clone(),
                    directory: directory.clone(),
                })
            })?
            .map(|(region, node)| RelayPlan {
                region,
                home: true,
                node,
                local_node: self.local_node,
                authorization: self.coordinator.data_plane().authorization_identity(),
                directory: self.coordinator.relay_identity(),
            }))
    }
    pub(crate) fn relay_alternatives(
        &mut self,
        plan: &RelayPlan,
        now_ms: u64,
    ) -> Result<Vec<RelayPlan>, Error> {
        if !self.validate_relay_plan(plan, now_ms)? {
            return Ok(Vec::new());
        }
        Ok(self
            .coordinator
            .relay_region(plan.region)
            .into_iter()
            .flat_map(|region| &region.nodes)
            .filter(|node| node.can_relay())
            .map(|node| RelayPlan {
                node: node.clone(),
                ..plan.clone()
            })
            .collect())
    }
    /// At most four distinct relay regions needed by the bounded output queue.
    pub fn pending_relay_plans(&mut self, now_ms: u64) -> Result<Vec<RelayPlan>, Error> {
        self.tick(now_ms)?;
        let candidates: Vec<_> = self
            .outgoing
            .iter()
            .map(|p| (p.peer, p.destination))
            .collect();
        let mut plans: Vec<RelayPlan> = Vec::new();
        for (peer, destination) in candidates {
            let needs_relay = match destination {
                Destination::Relay => true,
                Destination::Selected => matches!(
                    self.discovery.select(&mut self.coordinator, peer, now_ms)?,
                    Some(Route::Relay(_))
                ),
                _ => false,
            };
            if needs_relay
                && let Some(plan) = self.relay_plan(peer, now_ms)?
                && !plans.iter().any(|p| p.region == plan.region)
            {
                plans.push(plan);
                if plans.len() == 4 {
                    break;
                }
            }
        }
        Ok(plans)
    }
    pub fn validate_relay_plan(&mut self, plan: &RelayPlan, now_ms: u64) -> Result<bool, Error> {
        self.tick(now_ms)?;
        if !self.coordinator.contains_relay(plan.region, &plan.node)
            || (plan.home && !self.coordinator.allows_home_relay(plan.region, &plan.node))
        {
            return Ok(false);
        }
        Ok(self.coordinator.local_address(now_ms)?.is_some()
            && plan.local_node == self.local_node
            && std::sync::Arc::ptr_eq(&plan.directory, &self.coordinator.relay_identity())
            && std::sync::Arc::ptr_eq(
                &plan.authorization,
                &self.coordinator.data_plane().authorization_identity(),
            ))
    }
    pub fn validate_delivery(
        &mut self,
        permit: &DeliveryPermit,
        now_ms: u64,
    ) -> Result<bool, Error> {
        self.tick(now_ms)?;
        if permit
            .udp_socket
            .is_some_and(|socket| self.udp_bindings.port(socket).is_err())
        {
            return Ok(false);
        }
        if permit
            .tcp_socket
            .as_ref()
            .is_some_and(|socket| !tcp_owner::matches(&self.tcp_connections, socket))
        {
            return Ok(false);
        }
        if permit.deadline <= now_ms
            || self
                .coordinator
                .data_plane()
                .validate_handshake(&permit.permit, now_ms)
                .is_err()
        {
            return Ok(false);
        }
        Ok(match &permit.session {
            None => true,
            Some(token) => self
                .coordinator
                .data_plane()
                .session_identity(permit.peer, now_ms)?
                .is_some_and(|current| std::sync::Arc::ptr_eq(token, &current)),
        })
    }
    fn queue(
        &mut self,
        peer: Key,
        destination: Destination,
        bytes: Vec<u8>,
        now_ms: u64,
    ) -> Result<(), Error> {
        if self.outgoing.len() >= OUTGOING {
            return Err(Error::Capacity);
        }
        if bytes.len() > crate::derp::MAX_DATAGRAM {
            return Err(Error::Packet);
        }
        let permit = self
            .coordinator
            .data_plane()
            .begin_handshake(peer, now_ms)?;
        let deadline = now_ms.checked_add(5000).ok_or(Error::Clock)?;
        let session = if bytes.starts_with(&4u32.to_le_bytes()) {
            self.coordinator
                .data_plane()
                .session_identity(peer, now_ms)?
        } else {
            None
        };
        self.outgoing.push_back(Outbound {
            keepalive_reply: false,
            peer,
            destination,
            bytes,
            permit,
            session,
            deadline,
            udp_socket: None,
            tcp_socket: None,
        });
        Ok(())
    }
    pub fn initiate(&mut self, peer: Key, now_ms: u64) -> Result<(), Error> {
        self.tick(now_ms)?;
        self.queue_initiation(peer, now_ms)
    }
    fn queue_initiation(&mut self, peer: Key, now_ms: u64) -> Result<(), Error> {
        if self.outgoing.len() >= OUTGOING {
            return Err(Error::Capacity);
        }
        let packet = self.handshakes.begin(
            self.coordinator.data_plane(),
            peer,
            &mut self.timestamps,
            now_ms,
        )?;
        self.queue(peer, Destination::Selected, packet.to_vec(), now_ms)
    }
    /// True means an authorized, unexpired session exists. False means a
    /// no session is ready and a handshake is pending or rate-limited. An aging
    /// initiated session remains usable while its paced replacement is pending.
    /// Call again while
    /// polling transports; no application plaintext is retained here.
    pub fn ensure_session(&mut self, peer: Key, now_ms: u64) -> Result<bool, Error> {
        self.tick(now_ms)?;
        self.coordinator
            .data_plane()
            .begin_handshake(peer, now_ms)?;
        let active = self
            .coordinator
            .data_plane()
            .session_identity(peer, now_ms)?
            .is_some();
        if active
            && !self
                .coordinator
                .data_plane()
                .session_needs_rekey(peer, now_ms)?
        {
            return Ok(true);
        }
        match self.schedule_handshake(peer, now_ms) {
            Ok(_) => Ok(active),
            Err(Error::Capacity | Error::Handshake(crate::handshake::Error::Capacity))
                if active =>
            {
                Ok(true)
            }
            Err(error) => Err(error),
        }
    }
    fn schedule_handshake(&mut self, peer: Key, now_ms: u64) -> Result<bool, Error> {
        if self.handshakes.pending_for(peer)
            || now_ms < self.next_handshake_ms
            || self.handshake_retry.contains_key(&peer)
        {
            return Ok(false);
        }
        if self.handshake_retry.len() >= 256
            || (!self.retry_series.contains_key(&peer) && self.retry_series.len() >= 256)
        {
            return Err(Error::Capacity);
        }
        let global = now_ms.checked_add(100).ok_or(Error::Clock)?;
        let mut random = [0; 4];
        getrandom::getrandom(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
        let delay = 5000 + u64::from(u32::from_le_bytes(random) % 334);
        let retry = now_ms.checked_add(delay).ok_or(Error::Clock)?;
        let series = if self.retry_series.contains_key(&peer) {
            None
        } else {
            Some(RetrySeries {
                expires: now_ms.checked_add(90_000).ok_or(Error::Clock)?,
                authorization: self.coordinator.data_plane().authorization_identity(),
                session: self
                    .coordinator
                    .data_plane()
                    .session_identity(peer, now_ms)?,
            })
        };
        self.queue_initiation(peer, now_ms)?;
        if let Some(series) = series {
            self.retry_series.insert(peer, series);
        }
        self.next_handshake_ms = global;
        self.handshake_retry.insert(peer, retry);
        Ok(true)
    }
    pub fn send_ip(&mut self, peer: Key, packet: &[u8], now_ms: u64) -> Result<(), Error> {
        self.tick(now_ms)?;
        if self.outgoing.len() >= OUTGOING {
            return Err(Error::Capacity);
        }
        let packet = self.coordinator.data_plane().send(peer, packet, now_ms)?;
        self.queue(peer, Destination::Selected, packet, now_ms)
    }
    /// Binds on the current assigned IPv4 address. Port zero allocates a local
    /// ephemeral port. Disconnect invalidates all handles; maps retain bindings.
    pub fn bind_udp(&mut self, port: u16, now_ms: u64) -> Result<crate::udp::Socket, Error> {
        if self.local_address(now_ms)?.is_none() {
            return Err(crate::udp::Error::Closed.into());
        }
        let socket = self.udp_bindings.bind(port)?;
        let port = self.udp_bindings.port(socket)?;
        self.incoming
            .retain(|(_, _, destination)| *destination != Some(port));
        Ok(socket)
    }
    pub fn udp_port(&mut self, socket: crate::udp::Socket, now_ms: u64) -> Result<u16, Error> {
        self.tick(now_ms)?;
        Ok(self.udp_bindings.port(socket)?)
    }
    pub fn close_udp(&mut self, socket: crate::udp::Socket) -> Result<(), Error> {
        let port = self.udp_bindings.close(socket)?;
        self.coordinator.data_plane().revoke_udp_replies(socket);
        self.incoming
            .retain(|(_, _, destination)| *destination != Some(port));
        self.outgoing
            .retain(|packet| packet.udp_socket != Some(socket));
        Ok(())
    }
    /// Requires an established WireGuard session. Failed sends retain no
    /// application plaintext; the caller can initiate the peer and retry.
    pub fn send_udp(
        &mut self,
        socket: crate::udp::Socket,
        peer: Key,
        destination: std::net::SocketAddrV4,
        payload: &[u8],
        now_ms: u64,
    ) -> Result<(), Error> {
        let address = self
            .local_address(now_ms)?
            .ok_or(crate::udp::Error::Closed)?;
        let port = self.udp_bindings.port(socket)?;
        let packet = crate::udp::encode(
            std::net::SocketAddrV4::new(address, port),
            destination,
            payload,
        )?;
        let reply = self.coordinator.data_plane().prepare_udp_reply(
            peer,
            socket,
            std::net::SocketAddrV4::new(address, port),
            destination,
            now_ms,
        )?;
        self.send_ip(peer, &packet, now_ms)?;
        self.outgoing.back_mut().ok_or(Error::Packet)?.udp_socket = Some(socket);
        if let Err(error) = self.coordinator.data_plane().commit_udp_reply(reply) {
            self.outgoing.pop_back();
            return Err(error.into());
        }
        Ok(())
    }
    pub fn receive_udp(
        &mut self,
        socket: crate::udp::Socket,
        now_ms: u64,
    ) -> Result<Option<crate::udp::Datagram>, Error> {
        self.tick(now_ms)?;
        let port = self.udp_bindings.port(socket)?;
        let Some(index) = self
            .incoming
            .iter()
            .position(|(_, _, destination)| *destination == Some(port))
        else {
            return Ok(None);
        };
        let (peer, packet, _) = self.incoming.remove(index).ok_or(Error::Packet)?;
        let bytes = self.coordinator.data_plane().release(packet, now_ms)?;
        let udp = crate::udp::decode(&bytes)?;
        Ok(Some(crate::udp::Datagram {
            peer,
            source: udp.source,
            payload: udp.payload.to_vec(),
        }))
    }
    /// Queues the UDP payload when a session is ready, otherwise starts or
    /// waits for a paced handshake and returns false. The caller retains the
    /// payload until true; this function does not buffer it between calls.
    pub fn try_send_udp(
        &mut self,
        socket: crate::udp::Socket,
        peer: Key,
        destination: std::net::SocketAddrV4,
        payload: &[u8],
        now_ms: u64,
    ) -> Result<bool, Error> {
        let address = self
            .local_address(now_ms)?
            .ok_or(crate::udp::Error::Closed)?;
        let port = self.udp_bindings.port(socket)?;
        let packet = crate::udp::encode(
            std::net::SocketAddrV4::new(address, port),
            destination,
            payload,
        )?;
        self.coordinator
            .data_plane()
            .validate_outgoing(peer, &packet, now_ms)?;
        if !self.ensure_session(peer, now_ms)? {
            return Ok(false);
        }
        let reply = self.coordinator.data_plane().prepare_udp_reply(
            peer,
            socket,
            std::net::SocketAddrV4::new(address, port),
            destination,
            now_ms,
        )?;
        self.send_ip(peer, &packet, now_ms)?;
        self.outgoing.back_mut().ok_or(Error::Packet)?.udp_socket = Some(socket);
        if let Err(error) = self.coordinator.data_plane().commit_udp_reply(reply) {
            self.outgoing.pop_back();
            return Err(error.into());
        }
        Ok(true)
    }
    pub fn probe(&mut self, peer: Key, now_ms: u64) -> Result<bool, Error> {
        self.tick(now_ms)?;
        if self.outgoing.len() >= OUTGOING {
            return Err(Error::Capacity);
        }
        let Some(probe) = self
            .discovery
            .next_probe(&mut self.coordinator, peer, now_ms)?
        else {
            return Ok(false);
        };
        self.queue(
            peer,
            Destination::Probe(probe.endpoint),
            probe.packet,
            now_ms,
        )?;
        Ok(true)
    }
    /// Visits at most one authorized discovery peer per call, cycling through
    /// node IDs. Successful probes are at least 50 ms apart; unsuccessful scans
    /// wait five milliseconds. Endpoint rotation and refresh use Discovery.
    pub fn schedule_probe(&mut self, now_ms: u64) -> Result<bool, Error> {
        self.tick(now_ms)?;
        if now_ms < self.next_probe_scan_ms || self.outgoing.len() >= OUTGOING {
            return Ok(false);
        }
        self.next_probe_scan_ms = now_ms.checked_add(5).ok_or(Error::Clock)?;
        let Some((id, peer)) = self
            .coordinator
            .discovery_peer_after(self.probe_cursor, now_ms)?
        else {
            self.probe_cursor = None;
            return Ok(false);
        };
        self.probe_cursor = Some(id);
        match self.probe(peer, now_ms) {
            Ok(true) => {
                self.next_probe_scan_ms = now_ms.checked_add(50).ok_or(Error::Clock)?;
                Ok(true)
            }
            Ok(false)
            | Err(Error::Discovery(
                crate::discovery::Error::Invalid
                | crate::discovery::Error::Unknown
                | crate::discovery::Error::Capacity
                | crate::discovery::Error::Map(MapError::Node(_)),
            )) => Ok(false),
            Err(error) => Err(error),
        }
    }
    pub fn advertise(
        &mut self,
        peer: Key,
        endpoints: &[SocketAddr],
        now_ms: u64,
    ) -> Result<(), Error> {
        self.tick(now_ms)?;
        if self.outgoing.len() >= OUTGOING {
            return Err(Error::Capacity);
        }
        let packet =
            self.discovery
                .call_me_maybe(&mut self.coordinator, peer, endpoints, now_ms)?;
        self.queue(peer, Destination::Relay, packet, now_ms)
    }
    pub fn receive(&mut self, source: Source, packet: &[u8], now_ms: u64) -> Result<(), Error> {
        self.diagnostics.receive_calls = self.diagnostics.receive_calls.saturating_add(1);
        let result = self.receive_inner(source, packet, now_ms);
        if result.is_err() {
            self.diagnostics.receive_rejected = self.diagnostics.receive_rejected.saturating_add(1);
        }
        result
    }
    fn receive_inner(&mut self, source: Source, packet: &[u8], now_ms: u64) -> Result<(), Error> {
        self.tick(now_ms)?;
        if packet.len() > crate::derp::MAX_DATAGRAM {
            return Err(Error::Packet);
        }
        if packet.len() >= 8 && packet[0] & 0xc0 == 0 && packet[4..8] == [0x21, 0x12, 0xa4, 0x42] {
            let Source::Udp(SocketAddr::V4(server)) = source else {
                return Err(Error::Packet);
            };
            self.endpoints.receive(server, packet, now_ms)?;
            return Ok(());
        }
        if packet.starts_with(b"TS\xf0\x9f\x92\xac") {
            match source {
                Source::Relay(peer) => {
                    self.discovery.receive_call_me_maybe(
                        &mut self.coordinator,
                        peer,
                        packet,
                        now_ms,
                    )?;
                }
                Source::Udp(endpoint) => {
                    if let Ok((peer, reply)) =
                        self.discovery
                            .reply(&mut self.coordinator, endpoint, packet, now_ms)
                    {
                        self.queue(peer, Destination::UdpReply(endpoint), reply, now_ms)?;
                    } else {
                        self.discovery
                            .pong(&mut self.coordinator, endpoint, packet, now_ms)?;
                    }
                }
            }
            return Ok(());
        }
        let event = self
            .handshakes
            .handle(self.coordinator.data_plane(), packet, now_ms)?;
        let peer = match &event {
            Event::Response { peer, .. }
            | Event::Transport { peer, .. }
            | Event::Established(peer) => *peer,
        };
        if matches!(source,Source::Relay(label) if label!=peer) {
            return Err(Error::Packet);
        }
        self.silence.remove(&peer);
        let destination = match source {
            Source::Udp(endpoint) => Destination::UdpReply(endpoint),
            Source::Relay(_) => Destination::Selected,
        };
        let received_transport = matches!(&event, Event::Transport { .. });
        match event {
            Event::Response { packet, .. } => {
                self.queue(peer, destination, packet.to_vec(), now_ms)?
            }
            Event::Established(_) => {
                let packet = self.coordinator.data_plane().send(peer, &[], now_ms)?;
                self.queue(peer, destination, packet, now_ms)?;
            }
            Event::Transport {
                packet: Some(packet),
                ..
            } => {
                if self.incoming.len() >= INCOMING {
                    // Nothing consumes a packet that no open flow or bound
                    // socket claims, such as a late segment of a closed
                    // connection; left queued, eight of them block all input.
                    let unclaimed = self.incoming.iter().position(|(peer, packet, port)| {
                        match (packet.tcp_endpoints(), port) {
                            (Some((remote, local)), _) => !self
                                .tcp_connections
                                .values()
                                .any(|flow| flow.claims(peer, remote, local)),
                            (None, Some(port)) => !self.udp_bindings.is_bound(*port),
                            (None, None) => true,
                        }
                    });
                    let Some(index) = unclaimed else {
                        return Err(Error::Capacity);
                    };
                    self.incoming.remove(index);
                }
                let port = packet.udp_destination()?;
                self.arm_keepalive(peer, now_ms)?;
                self.incoming.push_back((peer, packet, port));
            }
            Event::Transport { packet: None, .. } => (),
        }
        if received_transport
            && self
                .coordinator
                .data_plane()
                .session_needs_receive_rekey(peer, now_ms)?
        {
            // Scheduling failure must not discard accepted inbound data. A later
            // authenticated packet or application readiness poll can retry.
            let _ = self.ensure_session(peer, now_ms);
        }
        Ok(())
    }
    pub fn receive_ip(&mut self, now_ms: u64) -> Result<Option<(Key, Vec<u8>)>, Error> {
        self.tick(now_ms)?;
        let Some((peer, packet, _)) = self.incoming.pop_front() else {
            return Ok(None);
        };
        Ok(Some((
            peer,
            self.coordinator.data_plane().release(packet, now_ms)?,
        )))
    }
    /// Invokes one nonblocking transport send under exclusive ownership. A
    /// WouldBlock retains work. Failed discovery probes are discarded; their
    /// scheduler supplies a fresh attempt. Other failures retain queued work.
    /// Selection and authorization are rechecked on every retry.
    pub fn flush_one(
        &mut self,
        now_ms: u64,
        mut send: impl FnMut(Route, Key, &[u8]) -> io::Result<()>,
    ) -> Result<bool, Error> {
        self.flush_one_authorized(now_ms, |route, peer, bytes, _| send(route, peer, bytes))
    }
    /// A retaining adapter must validate the supplied permit before later I/O.
    pub fn flush_one_authorized(
        &mut self,
        now_ms: u64,
        mut send: impl FnMut(Route, Key, &[u8], &DeliveryPermit) -> io::Result<()>,
    ) -> Result<bool, Error> {
        self.tick(now_ms)?;
        for _ in 0..self.outgoing.len() {
            let packet = self.outgoing.pop_front().ok_or(Error::Packet)?;
            let route = match packet.destination {
                Destination::Selected => {
                    self.discovery
                        .select(&mut self.coordinator, packet.peer, now_ms)?
                }
                Destination::UdpReply(endpoint) => Some(Route::Direct(endpoint)),
                Destination::Probe(endpoint) => self
                    .discovery
                    .candidates(&mut self.coordinator, packet.peer, now_ms)?
                    .contains(&endpoint)
                    .then_some(Route::Direct(endpoint)),
                Destination::Relay => {
                    let region = self
                        .coordinator
                        .route(packet.peer, now_ms)?
                        .and_then(|r| r.relay_region);
                    if self
                        .coordinator
                        .relay_for(packet.peer, now_ms)?
                        .is_some_and(|r| r.nodes.iter().any(|n| !n.stun_only))
                    {
                        region.map(Route::Relay)
                    } else {
                        None
                    }
                }
            };
            let Some(route) = route else {
                self.outgoing.push_back(packet);
                continue;
            };
            let permit = DeliveryPermit {
                peer: packet.peer,
                permit: packet.permit.clone(),
                session: packet.session.clone(),
                deadline: packet.deadline,
                udp_socket: packet.udp_socket,
                tcp_socket: packet.tcp_socket.clone(),
            };
            match send(route, packet.peer, &packet.bytes, &permit) {
                Ok(()) => {
                    self.diagnostics.send_accepted =
                        self.diagnostics.send_accepted.saturating_add(1);
                    if packet.session.is_some() && packet.bytes.len() > 32 {
                        // Output was accepted; timer setup cannot undo the send.
                        let _ = self.arm_silence(&packet, now_ms);
                    }
                    if packet.authenticated() {
                        if packet.keepalive_reply
                            && self
                                .keepalives
                                .get(&packet.peer)
                                .is_some_and(|timer| timer.again)
                        {
                            if let Some(deadline) = now_ms.checked_add(10_000) {
                                let timer = self
                                    .keepalives
                                    .get_mut(&packet.peer)
                                    .expect("keepalive timer");
                                timer.deadline = deadline;
                                timer.again = false;
                                timer.queued = false;
                            } else {
                                self.keepalives.remove(&packet.peer);
                            }
                        } else {
                            self.keepalives.remove(&packet.peer);
                        }
                    }
                    if packet.session.is_some() {
                        // Egress accepted this transport packet. Renewal is
                        // best effort and cannot undo that acceptance.
                        let _ = self.ensure_session(packet.peer, now_ms);
                    }
                    return Ok(true);
                }
                Err(error) => {
                    if error.kind() == io::ErrorKind::WouldBlock {
                        self.diagnostics.send_would_block =
                            self.diagnostics.send_would_block.saturating_add(1);
                    } else {
                        self.diagnostics.send_errors =
                            self.diagnostics.send_errors.saturating_add(1);
                    }
                    if error.kind() != io::ErrorKind::WouldBlock
                        && let Route::Direct(endpoint) = route
                    {
                        self.discovery.failed_endpoint(packet.peer, endpoint);
                    }
                    if error.kind() == io::ErrorKind::WouldBlock
                        || !matches!(packet.destination, Destination::Probe(_))
                    {
                        self.outgoing.push_back(packet);
                    }
                    if error.kind() == io::ErrorKind::WouldBlock {
                        return Ok(false);
                    }
                    return Err(error.into());
                }
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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
    fn clock(n: u64) -> Clock {
        Clock {
            unix_ms: 1_735_689_600_000 + n as i64,
            monotonic_ms: n,
        }
    }
    fn network(
        local: &DeviceIdentity,
        remote: &DeviceIdentity,
        ip: u8,
        other: u8,
    ) -> Network<Store> {
        let mut network =
            Network::new(local, TimestampClock::open(Store, 100).unwrap(), 4, 2).unwrap();
        let local_ip = format!("100.64.0.{ip}");
        let remote_ip = format!("100.64.0.{other}");
        let map = json!({"Node":{"ID":1,"Key":local.node_key().public().to_string(),"Addresses":[format!("{local_ip}/32")],"MachineAuthorized":true},"Peers":[{"ID":2,"Key":remote.node_key().public().to_string(),"DiscoKey":remote.disco_key().public().to_string(),"Addresses":[format!("{remote_ip}/32")],"Endpoints":[format!("192.0.2.{other}:1234")],"HomeDERP":7,"MachineAuthorized":true}],"DERPMap":{"Regions":{"7":{"RegionID":7,"Nodes":[{"Name":"7a","RegionID":7,"HostName":"relay.example"}]}}},"PacketFilter":[{"SrcIPs":[remote_ip],"DstPorts":[{"IP":local_ip,"Ports":{"First":1234,"Last":1234}}],"IPProto":[17]}]});
        network
            .apply_map(&serde_json::to_vec(&map).unwrap(), clock(0))
            .unwrap();
        network
    }
    fn jail_remote(network: &mut Network<Store>, remote: &DeviceIdentity) {
        let update = json!({"PeersChanged":[{"ID":2,"Key":remote.node_key().public().to_string(),"DiscoKey":remote.disco_key().public().to_string(),"Addresses":["100.64.0.2/32"],"Endpoints":["192.0.2.2:1234"],"HomeDERP":7,"MachineAuthorized":true,"IsJailed":true}]});
        network
            .apply_map(&serde_json::to_vec(&update).unwrap(), clock(0))
            .unwrap();
    }
    fn deliver(
        from: &mut Network<Store>,
        to: &mut Network<Store>,
        sender: Key,
        source: SocketAddr,
        now: u64,
    ) -> Route {
        let mut sent = None;
        assert!(
            from.flush_one(now, |route, _, bytes| {
                sent = Some((route, bytes.to_vec()));
                Ok(())
            })
            .unwrap()
        );
        let (route, bytes) = sent.unwrap();
        let ingress = match route {
            Route::Direct(_) => Source::Udp(source),
            Route::Relay(_) => Source::Relay(sender),
        };
        to.receive(ingress, &bytes, now).unwrap();
        route
    }
    fn udp() -> Vec<u8> {
        let mut ip = vec![0; 29];
        ip[0] = 0x45;
        ip[3] = 29;
        ip[8] = 64;
        ip[9] = 17;
        ip[12..16].copy_from_slice(&[100, 64, 0, 1]);
        ip[16..20].copy_from_slice(&[100, 64, 0, 2]);
        ip[20..22].copy_from_slice(&9000u16.to_be_bytes());
        ip[22..24].copy_from_slice(&1234u16.to_be_bytes());
        ip[25] = 9;
        ip[28] = 42;
        let mut checksum: u32 = ip[..20]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u32::from(u16::from_be_bytes(*b)))
            .sum();
        while checksum > 0xffff {
            checksum = (checksum & 0xffff) + (checksum >> 16);
        }
        ip[10..12].copy_from_slice(&(!(checksum as u16)).to_be_bytes());
        ip
    }
    #[test]
    fn diagnostics_track_backpressure_failures_and_owner_lifetime_without_payloads() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let mut owner = network(&a, &b, 1, 2);
        let source = Source::Udp("192.0.2.2:1234".parse().unwrap());
        assert!(
            owner
                .receive(source, b"private invalid payload", 0)
                .is_err()
        );
        owner.initiate(b.node_key().public(), 0).unwrap();
        let queued = owner.diagnostics();
        assert_eq!(queued.outgoing_packets, 1);
        assert!(queued.outgoing_capacity_bytes > 0);
        assert!(
            !owner
                .flush_one(0, |_, _, _| Err(io::ErrorKind::WouldBlock.into()))
                .unwrap()
        );
        assert!(
            owner
                .flush_one(0, |_, _, _| Err(io::ErrorKind::NetworkUnreachable.into()))
                .is_err()
        );
        assert!(owner.flush_one(0, |_, _, _| Ok(())).unwrap());
        let stats = owner.diagnostics();
        assert_eq!((stats.receive_calls, stats.receive_rejected), (1, 1));
        assert_eq!(
            (
                stats.send_accepted,
                stats.send_would_block,
                stats.send_errors
            ),
            (1, 1, 1)
        );
        assert_eq!(stats.outgoing_packets, 0);
        assert_eq!(stats.outgoing_capacity_bytes, 0);
        assert!(!format!("{stats:?}").contains("private"));
        owner.disconnect();
        assert_eq!(
            owner.diagnostics(),
            Diagnostics {
                known_peers: 0,
                usable_peers: 0,
                ..stats
            }
        );
        owner.diagnostics.receive_calls = u64::MAX;
        assert!(owner.receive(source, b"invalid", 1).is_err());
        assert_eq!(owner.diagnostics().receive_calls, u64::MAX);
    }

    #[test]
    fn idle_tick_retires_expired_transport_key_owners_without_new_traffic() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        na.ensure_session(bk, 0).unwrap();
        deliver(&mut na, &mut nb, ak, "192.0.2.1:1234".parse().unwrap(), 1);
        deliver(&mut nb, &mut na, bk, "192.0.2.2:1234".parse().unwrap(), 2);
        deliver(&mut na, &mut nb, ak, "192.0.2.1:1234".parse().unwrap(), 3);
        let alice = std::sync::Arc::downgrade(
            &na.coordinator
                .data_plane()
                .session_identity(bk, 3)
                .unwrap()
                .unwrap(),
        );
        let bob = std::sync::Arc::downgrade(
            &nb.coordinator
                .data_plane()
                .session_identity(ak, 3)
                .unwrap()
                .unwrap(),
        );
        na.tick(180_001).unwrap();
        nb.tick(180_000).unwrap();
        assert!(alice.upgrade().is_some());
        assert!(bob.upgrade().is_some());
        na.tick(180_002).unwrap();
        nb.tick(180_001).unwrap();
        assert!(
            alice.upgrade().is_none(),
            "expired initiator key owner retained"
        );
        assert!(
            bob.upgrade().is_none(),
            "expired responder key owner retained"
        );
        assert!(na.coordinator.local_address(180_002).unwrap().is_some());
        assert!(nb.coordinator.local_address(180_001).unwrap().is_some());
    }

    #[test]
    fn home_relay_is_independent_of_peers_and_excludes_ineligible_regions() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let mut owner = network(&a, &b, 1, 2);
        owner.apply_map(br#"{"PeersRemoved":[2],"DERPMap":{"Regions":{
            "1":{"RegionID":1,"NoMeasureNoHome":true,"Nodes":[{"Name":"1a","RegionID":1,"HostName":"excluded.example"}]},
            "2":{"RegionID":2,"Nodes":[{"Name":"2a","RegionID":2,"HostName":"stun.example","STUNOnly":true}]},
            "3":{"RegionID":3,"Nodes":[{"Name":"3a","RegionID":3,"HostName":"disabled.example","IPv4":"none","IPv6":"none"}]},
            "70000":{"RegionID":70000,"Nodes":[{"Name":"7a","RegionID":70000,"HostName":"home.example"}]}
        }}}"#, clock(1)).unwrap();
        assert!(owner.pending_relay_plans(1).unwrap().is_empty());
        let plan = owner.home_relay_plan(1).unwrap().unwrap();
        assert!(plan.is_home());
        assert_eq!(plan.region(), 70000);
        assert_eq!(plan.node().host_name, "home.example");
        assert!(owner.validate_relay_plan(&plan, 1).unwrap());
        owner.apply_map(br#"{"KeepAlive":true}"#, clock(2)).unwrap();
        assert!(owner.validate_relay_plan(&plan, 2).unwrap());
        let plan = owner.home_relay_plan(2).unwrap().unwrap();
        assert_eq!(plan.region(), 70000);
        assert!(owner.validate_relay_plan(&plan, 2).unwrap());
        owner.apply_map(br#"{"DERPMap":{"Regions":{"70000":{"RegionID":70000,"NoMeasureNoHome":true,"Nodes":[{"Name":"7a","RegionID":70000,"HostName":"home.example"}]}}}}"#, clock(3)).unwrap();
        assert!(!owner.validate_relay_plan(&plan, 3).unwrap());
        assert!(owner.home_relay_plan(3).unwrap().is_none());
        owner.disconnect();
        assert!(owner.home_relay_plan(4).unwrap().is_none());
    }
    #[test]
    fn relay_continuity_preserves_unchanged_maps_but_not_revoked_then_restored_state() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let mut owner = network(&a, &b, 1, 2);
        let original = owner.home_relay_plan(0).unwrap().unwrap();
        let directory = br#"{"DERPMap":{"Regions":{"7":{"RegionID":7,"Nodes":[{"Name":"7a","RegionID":7,"HostName":"relay.example"}]}}}}"#;
        for (i, map) in [
            br#"{"KeepAlive":true}"#.as_slice(),
            directory,
            br#"{"PeersChangedPatch":[{"NodeID":2,"Endpoints":["192.0.2.99:1234"]}]}"#,
        ]
        .iter()
        .enumerate()
        {
            owner.apply_map(map, clock(i as u64 + 1)).unwrap();
            assert!(owner.validate_relay_plan(&original, i as u64 + 1).unwrap());
        }
        owner
            .apply_map(br#"{"DERPMap":{"Regions":{}}}"#, clock(4))
            .unwrap();
        owner.apply_map(directory, clock(5)).unwrap();
        assert!(!owner.validate_relay_plan(&original, 5).unwrap());
        let replacement = owner.home_relay_plan(5).unwrap().unwrap();
        assert!(owner.validate_relay_plan(&replacement, 5).unwrap());
        owner
            .apply_map(br#"{"PacketFilter":[]}"#, clock(6))
            .unwrap();
        owner.apply_map(br#"{"PacketFilter":[{"SrcIPs":["100.64.0.2"],"DstPorts":[{"IP":"100.64.0.1","Ports":{"First":1234,"Last":1234}}],"IPProto":[17]}]}"#, clock(7)).unwrap();
        assert!(!owner.validate_relay_plan(&replacement, 7).unwrap());
        let current = owner.home_relay_plan(7).unwrap().unwrap();
        assert!(owner.validate_relay_plan(&current, 7).unwrap());
        owner.disconnect();
        assert!(!owner.validate_relay_plan(&current, 8).unwrap());
    }
    #[test]
    fn home_relay_requires_local_authorization_and_current_directory() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let mut owner = network(&a, &b, 1, 2);
        let home = owner.home_relay_plan(0).unwrap().unwrap();
        let peer = owner.relay_plan(b.node_key().public(), 0).unwrap().unwrap();
        assert!(!peer.is_home());
        owner
            .apply_map(br#"{"DERPMap":{"Regions":{}}}"#, clock(1))
            .unwrap();
        assert!(!owner.validate_relay_plan(&home, 1).unwrap());
        assert!(!owner.validate_relay_plan(&peer, 1).unwrap());
        assert!(owner.home_relay_plan(1).unwrap().is_none());
        let mut owner = network(&a, &b, 1, 2);
        let update = json!({"Node":{"ID":1,"Key":a.node_key().public().to_string(),"Addresses":["100.64.0.1/32"],"MachineAuthorized":false}});
        assert!(
            owner
                .apply_map(&serde_json::to_vec(&update).unwrap(), clock(1))
                .is_err()
        );
        assert!(owner.home_relay_plan(1).unwrap().is_none());
    }
    #[test]
    fn heartbeat_preserves_queued_traffic_and_retained_delivery() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:1234".parse().unwrap();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        na.initiate(bk, 0).unwrap();
        na.apply_map(br#"{"KeepAlive":true}"#, clock(1)).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 2);
        deliver(&mut nb, &mut na, bk, eb, 3);
        deliver(&mut na, &mut nb, ak, ea, 4);
        na.send_ip(bk, &udp(), 5).unwrap();
        let permit = retain_delivery(&mut na, 5);
        na.send_ip(bk, &udp(), 6).unwrap();
        let heartbeat =
            br#"{"KeepAlive":true,"PeersRemoved":[2],"PacketFilter":[],"DERPMap":null}"#;
        na.apply_map(heartbeat, clock(7)).unwrap();
        assert!(na.validate_delivery(&permit, 7).unwrap());
        deliver(&mut na, &mut nb, ak, ea, 8);
        nb.apply_map(heartbeat, clock(9)).unwrap();
        assert_eq!(nb.receive_ip(9).unwrap(), Some((ak, udp())));
        na.apply_map(br#"{}"#, clock(10)).unwrap();
        assert!(!na.validate_delivery(&permit, 10).unwrap());
        nb.apply_map(br#"{"PacketFilter":[]}"#, clock(10)).unwrap();
        na.send_ip(bk, &udp(), 11).unwrap();
        let mut denied = None;
        assert!(
            na.flush_one(12, |_, _, bytes| {
                denied = Some(bytes.to_vec());
                Ok(())
            })
            .unwrap()
        );
        assert!(nb.receive(Source::Relay(ak), &denied.unwrap(), 12).is_err());
        assert!(nb.receive_ip(12).unwrap().is_none());
    }
    #[test]
    fn retained_delivery_checks_socket_session_deadline_and_map_lifetime() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:1234".parse().unwrap();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        let plan = na.relay_plan(bk, 0).unwrap().unwrap();
        assert_eq!(plan.region(), 7);
        assert!(na.validate_relay_plan(&plan, 0).unwrap());
        assert!(!nb.validate_relay_plan(&plan, 0).unwrap());
        na.initiate(bk, 0).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 1);
        deliver(&mut nb, &mut na, bk, eb, 2);
        deliver(&mut na, &mut nb, ak, ea, 3);
        let target = "100.64.0.2:1234".parse().unwrap();
        let socket = na.bind_udp(1234, 4).unwrap();
        na.send_udp(socket, bk, target, b"closed socket", 4)
            .unwrap();
        let permit = retain_delivery(&mut na, 4);
        assert!(na.validate_delivery(&permit, 4).unwrap());
        na.close_udp(socket).unwrap();
        assert!(!na.validate_delivery(&permit, 4).unwrap());
        let socket = na.bind_udp(1234, 4).unwrap();
        na.send_udp(socket, bk, target, b"old session", 4).unwrap();
        let permit = retain_delivery(&mut na, 4);
        na.initiate(bk, 5).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 6);
        deliver(&mut nb, &mut na, bk, eb, 7);
        deliver(&mut na, &mut nb, ak, ea, 8);
        assert!(!na.validate_delivery(&permit, 8).unwrap());
        assert!(na.validate_relay_plan(&plan, 8).unwrap());
        na.send_udp(socket, bk, target, b"deadline", 9).unwrap();
        let permit = retain_delivery(&mut na, 9);
        assert!(na.validate_delivery(&permit, 5008).unwrap());
        assert!(!na.validate_delivery(&permit, 5009).unwrap());
        na.apply_map(br#"{"PacketFilter":[]}"#, clock(5010))
            .unwrap();
        assert!(!na.validate_relay_plan(&plan, 5010).unwrap());
        let plan = na.relay_plan(bk, 5010).unwrap().unwrap();
        na.disconnect();
        assert!(!na.validate_relay_plan(&plan, 5011).unwrap());
    }
    fn retain_delivery(owner: &mut Network<Store>, now: u64) -> DeliveryPermit {
        let mut saved = None;
        assert!(
            owner
                .flush_one_authorized(now, |_, _, _, permit| {
                    saved = Some(permit.clone());
                    Ok(())
                })
                .unwrap()
        );
        saved.unwrap()
    }
    #[test]
    #[cfg(all(unix, not(target_os = "espidf")))]
    fn named_udp_request_runs_handshake_and_filters_response_source() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:1234".parse().unwrap();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        name_peer(&mut na, &b, 0);
        let mut echo = crate::host_network::UdpEcho::new(1234).unwrap();
        echo.poll(&mut nb, 0).unwrap();
        let wrong_socket = nb.bind_udp(1235, 0).unwrap();
        let mut request = crate::udp_request::Request::new(
            "PEER.EXAMPLE.",
            1234,
            1234,
            b"named request",
            0,
            10000,
        )
        .unwrap();
        assert!(request.poll(&mut na, 0).unwrap().is_none());
        deliver(&mut na, &mut nb, ak, ea, 1);
        deliver(&mut nb, &mut na, bk, eb, 2);
        deliver(&mut na, &mut nb, ak, ea, 3);
        assert!(request.poll(&mut na, 3).unwrap().is_none());
        deliver(&mut na, &mut nb, ak, ea, 4);
        nb.send_udp(
            wrong_socket,
            ak,
            "100.64.0.1:1234".parse().unwrap(),
            b"wrong source port",
            4,
        )
        .unwrap();
        deliver(&mut nb, &mut na, bk, eb, 4);
        assert!(request.poll(&mut na, 4).unwrap().is_none());
        echo.poll(&mut nb, 4).unwrap();
        deliver(&mut nb, &mut na, bk, eb, 5);
        assert_eq!(request.poll(&mut na, 5).unwrap().unwrap(), b"named request");
        assert!(matches!(
            request.poll(&mut na, 5),
            Err(crate::udp_request::Error::Finished)
        ));
        let mut cancelled = crate::udp_request::Request::new(
            "peer.example",
            1234,
            1234,
            b"cancel queued send",
            5,
            10000,
        )
        .unwrap();
        assert!(cancelled.poll(&mut na, 5).unwrap().is_none());
        cancelled.cancel(&mut na);
        assert!(
            !na.flush_one(5, |_, _, _| panic!("cancelled request sent"))
                .unwrap()
        );
        assert!(na.bind_udp(1234, 5).is_ok());
    }
    fn name_peer(owner: &mut Network<Store>, peer: &DeviceIdentity, now: u64) {
        owner.apply_map(&serde_json::to_vec(&json!({"PeersChanged":[{"ID":2,"Key":peer.node_key().public().to_string(),"Name":"peer.example.","Addresses":["100.64.0.2/32"],"MachineAuthorized":true,"HomeDERP":7}]})).unwrap(), clock(now)).unwrap();
    }
    #[test]
    fn named_udp_request_deadline_and_name_changes_are_terminal() {
        use crate::udp_request::{Error as RequestError, Request};
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let c = DeviceIdentity::generate().unwrap();
        let mut owner = network(&a, &b, 1, 2);
        let mut missing = Request::new("missing.example", 1234, 0, b"x", 0, 10).unwrap();
        assert!(missing.poll(&mut owner, 9).unwrap().is_none());
        assert!(matches!(
            missing.poll(&mut owner, 10),
            Err(RequestError::Timeout)
        ));
        name_peer(&mut owner, &b, 11);
        let mut changed = Request::new("peer.example", 1234, 1234, b"x", 11, 10000).unwrap();
        assert!(changed.poll(&mut owner, 11).unwrap().is_none());
        name_peer(&mut owner, &c, 12);
        assert!(matches!(
            changed.poll(&mut owner, 12),
            Err(RequestError::Changed)
        ));
        assert!(owner.bind_udp(1234, 12).is_ok());
        let mut backwards = Request::new("peer.example", 1234, 0, b"x", 20, 100).unwrap();
        assert!(matches!(
            backwards.poll(&mut owner, 19),
            Err(RequestError::Clock)
        ));
        assert!(Request::new("bad name", 1234, 0, b"x", 0, 100).is_err());
        assert!(
            Request::new(
                "peer.example",
                1234,
                0,
                &[0; crate::udp::MAX_PAYLOAD + 1],
                0,
                100
            )
            .is_err()
        );
        assert!(Request::new("peer.example", 1234, 0, b"x", 0, 60001).is_err());
    }
    #[test]
    fn initiated_session_renews_before_expiry_and_keeps_old_keys_until_response() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:1234".parse().unwrap();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        let sa = na.bind_udp(1234, 0).unwrap();
        let sb = nb.bind_udp(1234, 0).unwrap();
        let target = "100.64.0.2:1234".parse().unwrap();
        assert!(!na.ensure_session(bk, 0).unwrap());
        deliver(&mut na, &mut nb, ak, ea, 1);
        deliver(&mut nb, &mut na, bk, eb, 2);
        deliver(&mut na, &mut nb, ak, ea, 3);
        let old = na
            .coordinator
            .data_plane()
            .session_identity(bk, 3)
            .unwrap()
            .unwrap();
        assert!(na.ensure_session(bk, 120001).unwrap());
        assert_eq!(na.handshakes.pending_count(), 0);
        assert!(nb.ensure_session(ak, 120002).unwrap());
        assert_eq!(
            nb.handshakes.pending_count(),
            0,
            "responder must not initiate timed renewal"
        );
        assert!(na.ensure_session(bk, 120002).unwrap());
        assert_eq!(na.handshakes.pending_count(), 1);
        assert!(
            na.try_send_udp(sa, bk, target, b"old keys while pending", 120003)
                .unwrap()
        );
        assert_eq!(na.handshakes.pending_count(), 1);
        assert!(std::sync::Arc::ptr_eq(
            &old,
            &na.coordinator
                .data_plane()
                .session_identity(bk, 120003)
                .unwrap()
                .unwrap()
        ));
        deliver(&mut na, &mut nb, ak, ea, 120004);
        deliver(&mut na, &mut nb, ak, ea, 120004);
        assert_eq!(
            nb.receive_udp(sb, 120004).unwrap().unwrap().payload,
            b"old keys while pending"
        );
        deliver(&mut nb, &mut na, bk, eb, 120005);
        let renewed = na
            .coordinator
            .data_plane()
            .session_identity(bk, 120005)
            .unwrap()
            .unwrap();
        assert!(!std::sync::Arc::ptr_eq(&old, &renewed));
        deliver(&mut na, &mut nb, ak, ea, 120006);
        assert!(
            na.try_send_udp(sa, bk, target, b"after original expiry", 180003)
                .unwrap()
        );
        assert_eq!(na.handshakes.pending_count(), 0);
        deliver(&mut na, &mut nb, ak, ea, 180004);
        assert_eq!(
            nb.receive_udp(sb, 180004).unwrap().unwrap().payload,
            b"after original expiry"
        );
    }
    #[test]
    fn authenticated_receive_renews_only_aging_initiator_keys() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:1234".parse().unwrap();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        na.ensure_session(bk, 0).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 1);
        deliver(&mut nb, &mut na, bk, eb, 2);
        deliver(&mut na, &mut nb, ak, ea, 3);
        // The responder receives an old-session keepalive without initiating.
        let packet = na.coordinator.data_plane().send(bk, &[], 165002).unwrap();
        nb.receive(Source::Relay(ak), &packet, 165002).unwrap();
        assert!(nb.outgoing.is_empty());
        nb.send_ip(ak, &[], 165002).unwrap();
        deliver(&mut nb, &mut na, bk, eb, 165002);
        assert!(na.outgoing.is_empty());
        nb.send_ip(ak, &[], 165003).unwrap();
        let mut packet = Vec::new();
        nb.flush_one(165003, |_, _, bytes| {
            packet = bytes.to_vec();
            Ok(())
        })
        .unwrap();
        let mut bad = packet.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(na.receive(Source::Relay(bk), &bad, 165003).is_err());
        assert!(na.outgoing.is_empty());
        // A full outbound queue must not turn accepted inbound data into an error.
        let socket = na.bind_udp(1234, 165003).unwrap();
        let remote = nb.bind_udp(1234, 165003).unwrap();
        for _ in 0..OUTGOING {
            na.send_ip(bk, &[], 165003).unwrap();
        }
        nb.send_udp(
            remote,
            ak,
            "100.64.0.1:1234".parse().unwrap(),
            b"receive-only",
            165003,
        )
        .unwrap();
        deliver(&mut nb, &mut na, bk, eb, 165003);
        assert_eq!(
            na.receive_udp(socket, 165003).unwrap().unwrap().payload,
            b"receive-only"
        );
        assert_eq!(na.handshakes.pending_count(), 0);
        na.outgoing.clear();
        na.receive(Source::Relay(bk), &packet, 165003).unwrap();
        assert_eq!(na.handshakes.pending_count(), 1);
        assert_eq!(na.outgoing.len(), 1);
        assert!(na.receive(Source::Relay(bk), &packet, 165003).is_err());
        assert_eq!(na.outgoing.len(), 1);
        deliver(&mut na, &mut nb, ak, ea, 165004);
        deliver(&mut nb, &mut na, bk, eb, 165005);
        deliver(&mut na, &mut nb, ak, ea, 165006);
        assert_eq!(na.completed_handshakes().initiated, 2);
        nb.send_ip(ak, &[], 180003).unwrap();
        deliver(&mut nb, &mut na, bk, eb, 180003);
        assert!(na.outgoing.is_empty());
        assert!(na.ensure_session(bk, 180003).unwrap());
    }
    #[test]
    fn raw_transport_egress_requests_renewal_only_after_acceptance() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:1234".parse().unwrap();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        na.initiate(bk, 0).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 1);
        deliver(&mut nb, &mut na, bk, eb, 2);
        deliver(&mut na, &mut nb, ak, ea, 3);
        na.send_ip(bk, &[], 120002).unwrap();
        assert!(
            !na.flush_one(120002, |_, _, _| Err(io::ErrorKind::WouldBlock.into()))
                .unwrap()
        );
        assert_eq!(na.handshakes.pending_count(), 0);
        assert_eq!(na.outgoing.len(), 1);
        assert!(
            na.flush_one(120003, |_, _, _| Err(io::ErrorKind::Other.into()))
                .is_err()
        );
        assert_eq!(na.handshakes.pending_count(), 0);
        assert_eq!(na.outgoing.len(), 1);
        deliver(&mut na, &mut nb, ak, ea, 120004);
        assert_eq!(na.handshakes.pending_count(), 1);
        assert_eq!(na.outgoing.len(), 1);
        // Sending the handshake itself must not recursively request another.
        deliver(&mut na, &mut nb, ak, ea, 120005);
        assert!(na.outgoing.is_empty());
        deliver(&mut nb, &mut na, bk, eb, 120006);
        deliver(&mut na, &mut nb, ak, ea, 120007);
        assert_eq!(na.completed_handshakes().initiated, 2);
        na.send_ip(bk, &[], 180003).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 180003);
        assert!(na.outgoing.is_empty());
    }
    #[test]
    fn idle_receive_keepalive_is_delayed_coalesced_and_does_not_ping_pong() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:1234".parse().unwrap();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        let socket = nb.bind_udp(1234, 0).unwrap();
        na.initiate(bk, 0).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 1);
        deliver(&mut nb, &mut na, bk, eb, 2);
        deliver(&mut na, &mut nb, ak, ea, 3);
        assert!(na.keepalives.is_empty());
        assert!(nb.keepalives.is_empty());
        for now in [10, 20] {
            nb.send_udp(socket, ak, "100.64.0.1:1234".parse().unwrap(), b"data", now)
                .unwrap();
            deliver(&mut nb, &mut na, bk, eb, now);
        }
        assert_eq!(na.keepalives[&bk].deadline, 10010);
        na.apply_map(br#"{}"#, clock(21)).unwrap();
        assert_eq!(na.keepalives[&bk].deadline, 10010);
        na.tick(10009).unwrap();
        assert!(na.outgoing.is_empty());
        na.tick(10010).unwrap();
        assert_eq!(na.outgoing.len(), 1);
        assert_eq!(na.outgoing.front().unwrap().bytes.len(), 32);
        let before_map = na.outgoing.front().unwrap().bytes.clone();
        na.apply_map(br#"{}"#, clock(10010)).unwrap();
        assert_eq!(na.outgoing.len(), 1);
        assert_ne!(na.outgoing.front().unwrap().bytes, before_map);
        na.tick(10010).unwrap();
        assert_eq!(na.outgoing.len(), 1);
        assert!(
            !na.flush_one(10010, |_, _, _| Err(io::ErrorKind::WouldBlock.into()))
                .unwrap()
        );
        deliver(&mut na, &mut nb, ak, ea, 10011);
        assert!(nb.keepalives.is_empty());
        assert_eq!(na.keepalives[&bk].deadline, 20011);
        na.tick(20011).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 20011);
        assert!(na.keepalives.is_empty());
        nb.tick(30011).unwrap();
        na.tick(30011).unwrap();
        assert!(na.outgoing.is_empty());
        assert!(nb.outgoing.is_empty());
        // An ordinary reply cancels the timer; revocation also cancels queued keepalives.
        nb.send_udp(
            socket,
            ak,
            "100.64.0.1:1234".parse().unwrap(),
            b"new",
            30012,
        )
        .unwrap();
        deliver(&mut nb, &mut na, bk, eb, 30012);
        na.send_ip(bk, &[], 30013).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 30013);
        assert!(na.keepalives.is_empty());
        nb.send_udp(
            socket,
            ak,
            "100.64.0.1:1234".parse().unwrap(),
            b"last",
            30014,
        )
        .unwrap();
        deliver(&mut nb, &mut na, bk, eb, 30014);
        for _ in 0..OUTGOING {
            na.send_ip(bk, &[], 39999).unwrap();
        }
        na.tick(40014).unwrap();
        assert_eq!(na.outgoing.len(), OUTGOING);
        assert!(!na.keepalives[&bk].queued);
        na.outgoing.clear();
        na.tick(40014).unwrap();
        assert_eq!(na.outgoing.len(), 1);
        let previous = na.outgoing.front().unwrap().bytes.clone();
        na.tick(45014).unwrap();
        assert_eq!(na.outgoing.len(), 1);
        assert_ne!(na.outgoing.front().unwrap().bytes, previous);
        na.apply_map(br#"{"PeersRemoved":[2]}"#, clock(45014))
            .unwrap();
        assert!(na.keepalives.is_empty());
        assert!(
            !na.flush_one(45014, |_, _, _| panic!("revoked keepalive escaped"))
                .unwrap()
        );
    }
    #[test]
    fn unanswered_data_triggers_paced_handshake_and_authenticated_receive_cancels() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:1234".parse().unwrap();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        let socket = na.bind_udp(1234, 0).unwrap();
        let target = "100.64.0.2:1234".parse().unwrap();
        na.initiate(bk, 0).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 1);
        deliver(&mut nb, &mut na, bk, eb, 2);
        deliver(&mut na, &mut nb, ak, ea, 3);
        assert!(na.silence.is_empty());
        na.send_udp(socket, bk, target, b"lost", 10).unwrap();
        assert!(
            !na.flush_one(10, |_, _, _| Err(io::ErrorKind::WouldBlock.into()))
                .unwrap()
        );
        assert!(na.silence.is_empty());
        na.flush_one(10, |_, _, _| Ok(())).unwrap();
        let deadline = na.silence[&bk].deadline;
        assert!((15000..=15333).contains(&(deadline - 10)));
        na.send_udp(socket, bk, target, b"also lost", 11).unwrap();
        na.flush_one(11, |_, _, _| Ok(())).unwrap();
        assert_eq!(na.silence[&bk].deadline, deadline);
        na.apply_map(br#"{}"#, clock(11)).unwrap();
        assert_eq!(na.silence[&bk].deadline, deadline);
        nb.send_ip(ak, &[], 12).unwrap();
        let mut reply = Vec::new();
        nb.flush_one(12, |_, _, bytes| {
            reply = bytes.to_vec();
            Ok(())
        })
        .unwrap();
        let mut forged = reply.clone();
        *forged.last_mut().unwrap() ^= 1;
        assert!(na.receive(Source::Relay(bk), &forged, 12).is_err());
        assert!(na.silence.contains_key(&bk));
        na.receive(Source::Relay(bk), &reply, 12).unwrap();
        assert!(na.silence.is_empty());
        na.send_udp(socket, bk, target, b"unanswered", 20).unwrap();
        na.flush_one(20, |_, _, _| Ok(())).unwrap();
        assert!(na.receive(Source::Relay(bk), &reply, 21).is_err());
        assert!(na.silence.contains_key(&bk));
        let deadline = na.silence[&bk].deadline;
        na.tick(deadline - 1).unwrap();
        assert_eq!(na.handshakes.pending_count(), 0);
        for _ in 0..OUTGOING {
            na.send_ip(bk, &[], deadline - 1).unwrap();
        }
        na.tick(deadline).unwrap();
        assert!(na.silence.contains_key(&bk));
        assert_eq!(na.handshakes.pending_count(), 0);
        na.outgoing.clear();
        na.tick(deadline).unwrap();
        assert!(na.silence.is_empty());
        assert_eq!(na.handshakes.pending_count(), 1);
        assert_eq!(na.outgoing.len(), 1);
        deliver(&mut na, &mut nb, ak, ea, deadline + 1);
        deliver(&mut nb, &mut na, bk, eb, deadline + 2);
        deliver(&mut na, &mut nb, ak, ea, deadline + 3);
        assert_eq!(na.completed_handshakes().initiated, 2);
        na.send_udp(socket, bk, target, b"revoked", deadline + 4)
            .unwrap();
        na.flush_one(deadline + 4, |_, _, _| Ok(())).unwrap();
        assert!(na.silence.contains_key(&bk));
        na.apply_map(br#"{"PeersRemoved":[2]}"#, clock(deadline + 5))
            .unwrap();
        assert!(na.silence.is_empty());
        na.tick(deadline + 20000).unwrap();
        assert!(na.outgoing.is_empty());
    }
    #[test]
    fn renewal_defers_when_full_paces_failure_and_obeys_peer_revocation() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:1234".parse().unwrap();
        let sa = na.bind_udp(1234, 0).unwrap();
        assert!(!na.ensure_session(bk, 0).unwrap());
        deliver(&mut na, &mut nb, ak, ea, 1);
        deliver(&mut nb, &mut na, bk, eb, 2);
        deliver(&mut na, &mut nb, ak, ea, 3);
        for _ in 0..OUTGOING {
            na.send_udp(sa, bk, "100.64.0.2:1234".parse().unwrap(), b"full", 120001)
                .unwrap();
        }
        assert!(na.ensure_session(bk, 120002).unwrap());
        assert_eq!(na.handshakes.pending_count(), 0);
        na.outgoing.clear();
        assert!(na.ensure_session(bk, 120003).unwrap());
        let mut initiation = Vec::new();
        assert!(
            na.flush_one(120003, |_, _, bytes| {
                initiation = bytes.to_vec();
                Ok(())
            })
            .unwrap()
        );
        let mut forged = [0; 92];
        forged[0] = 2;
        forged[8..12].copy_from_slice(&initiation[4..8]);
        assert!(na.receive(Source::Relay(bk), &forged, 120004).is_err());
        assert_eq!(na.handshakes.pending_count(), 0);
        assert!(na.ensure_session(bk, 125002).unwrap());
        assert!(na.outgoing.is_empty());
        assert!(na.ensure_session(bk, 125336).unwrap());
        assert_eq!(na.handshakes.pending_count(), 1);
        na.apply_map(br#"{"PeersRemoved":[2]}"#, clock(125337))
            .unwrap();
        assert!(na.ensure_session(bk, 125337).is_err());
        assert_eq!(na.handshakes.pending_count(), 0);
        assert!(na.outgoing.is_empty());
    }
    #[test]
    fn udp_retry_paces_handshakes_and_reconnects_expired_sessions() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:1234".parse().unwrap();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        let sa = na.bind_udp(1234, 0).unwrap();
        let sb = nb.bind_udp(1234, 0).unwrap();
        let target = "100.64.0.2:1234".parse().unwrap();
        assert!(
            na.try_send_udp(sa, bk, "100.64.0.3:1234".parse().unwrap(), b"wrong peer", 0)
                .is_err()
        );
        assert!(na.outgoing.is_empty());
        assert!(
            !na.try_send_udp(sa, bk, target, b"discarded attempt payload", 0)
                .unwrap()
        );
        assert!(
            !na.try_send_udp(sa, bk, target, b"also not buffered", 1)
                .unwrap()
        );
        assert_eq!(na.handshakes.pending_count(), 1);
        let mut initiation = Vec::new();
        assert!(
            na.flush_one(1, |_, _, bytes| {
                initiation = bytes.to_vec();
                Ok(())
            })
            .unwrap()
        );
        let mut bad_response = [0; 92];
        bad_response[0] = 2;
        bad_response[8..12].copy_from_slice(&initiation[4..8]);
        assert!(na.receive(Source::Relay(bk), &bad_response, 2).is_err());
        assert_eq!(na.handshakes.pending_count(), 0);
        assert!(!na.ensure_session(bk, 4999).unwrap());
        assert!(na.outgoing.is_empty());
        assert!(!na.ensure_session(bk, 5333).unwrap());
        deliver(&mut na, &mut nb, ak, ea, 5334);
        deliver(&mut nb, &mut na, bk, eb, 5335);
        deliver(&mut na, &mut nb, ak, ea, 5336);
        assert!(na.ensure_session(bk, 5336).unwrap());
        assert!(nb.receive_udp(sb, 5336).unwrap().is_none());
        assert!(
            na.try_send_udp(sa, bk, target, b"explicit retry", 5337)
                .unwrap()
        );
        deliver(&mut na, &mut nb, ak, ea, 5338);
        assert_eq!(
            nb.receive_udp(sb, 5338).unwrap().unwrap().payload,
            b"explicit retry"
        );
        assert!(!na.ensure_session(bk, 185335).unwrap());
        assert_eq!(na.handshakes.pending_count(), 1);
        na.apply_map(br#"{"PeersRemoved":[2]}"#, clock(185336))
            .unwrap();
        assert!(na.ensure_session(bk, 185336).is_err());
        assert!(na.outgoing.is_empty());
        assert!(na.handshake_retry.is_empty());
    }
    #[test]
    fn handshake_readiness_limits_global_initiation_rate() {
        let local = DeviceIdentity::generate().unwrap();
        let first = DeviceIdentity::generate().unwrap();
        let second = DeviceIdentity::generate().unwrap();
        let mut owner = network(&local, &first, 1, 2);
        let second_key = second.node_key().public();
        owner.apply_map(&serde_json::to_vec(&json!({"PeersChanged":[{"ID":3,"Key":second_key.to_string(),"Addresses":["100.64.0.3/32"],"MachineAuthorized":true}]})).unwrap(), clock(0)).unwrap();
        assert!(!owner.ensure_session(first.node_key().public(), 0).unwrap());
        assert!(!owner.ensure_session(second_key, 99).unwrap());
        assert_eq!(owner.handshakes.pending_count(), 1);
        assert!(!owner.ensure_session(second_key, 100).unwrap());
        assert_eq!(owner.handshakes.pending_count(), 2);
    }
    #[test]
    fn tick_recovers_lost_handshake_without_application_demand() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        assert!(!na.ensure_session(bk, 0).unwrap());
        let deadline = na.handshake_retry[&bk];
        na.flush_one(1, |_, _, _| Ok(())).unwrap(); // Lost initiation.
        // Harmless coordinator deltas invalidate packet permits, but must not
        // erase retry intent under unchanged authorization.
        na.apply_map(br#"{}"#, clock(2)).unwrap();
        assert_eq!(na.retry_series.len(), 1);
        na.tick(deadline - 1).unwrap();
        assert!(na.outgoing.is_empty());
        na.tick(deadline).unwrap();
        assert_eq!(na.outgoing.len(), 1);
        deliver(
            &mut na,
            &mut nb,
            ak,
            "192.0.2.1:1234".parse().unwrap(),
            deadline + 1,
        );
        nb.flush_one(deadline + 2, |_, _, _| Ok(())).unwrap(); // Lost response.
        let deadline = na.handshake_retry[&bk];
        na.tick(deadline).unwrap();
        assert_eq!(na.outgoing.len(), 1);
        deliver(
            &mut na,
            &mut nb,
            ak,
            "192.0.2.1:1234".parse().unwrap(),
            deadline + 1,
        );
        deliver(
            &mut nb,
            &mut na,
            bk,
            "192.0.2.2:1234".parse().unwrap(),
            deadline + 2,
        );
        deliver(
            &mut na,
            &mut nb,
            ak,
            "192.0.2.1:1234".parse().unwrap(),
            deadline + 3,
        );
        na.tick(deadline + 6000).unwrap();
        assert!(na.retry_series.is_empty());
        assert!(na.outgoing.is_empty());
        assert_eq!(na.completed_handshakes().initiated, 1);
    }
    #[test]
    fn autonomous_handshake_series_has_fixed_budget_and_revocation() {
        let local = DeviceIdentity::generate().unwrap();
        let peer = DeviceIdentity::generate().unwrap();
        let key = peer.node_key().public();
        let mut owner = network(&local, &peer, 1, 2);
        assert!(!owner.ensure_session(key, 0).unwrap());
        let mut attempts = 1;
        while owner.handshake_retry[&key] < 90_000 {
            let deadline = owner.handshake_retry[&key];
            owner.outgoing.clear();
            owner.tick(deadline).unwrap();
            assert_eq!(owner.outgoing.len(), 1);
            assert_eq!(owner.retry_series[&key].expires, 90_000);
            attempts += 1;
        }
        assert!(attempts <= 18);
        owner.outgoing.clear();
        owner.tick(90_000).unwrap();
        owner.tick(100_000).unwrap();
        assert!(owner.retry_series.is_empty());
        assert!(owner.outgoing.is_empty());
        assert_eq!(owner.handshakes.pending_count(), 0);
        // Fresh application demand starts a new bounded series.
        assert!(!owner.ensure_session(key, 100_000).unwrap());
        let deadline = owner.handshake_retry[&key];
        owner.outgoing.clear();
        // Keep the queue full with fresh packets across the retry deadline.
        for _ in 0..OUTGOING {
            owner
                .queue(key, Destination::Selected, vec![0; 32], deadline - 1)
                .unwrap();
        }
        owner.tick(deadline).unwrap();
        assert_eq!(owner.handshakes.pending_count(), 0);
        assert_eq!(owner.retry_series[&key].expires, 190_000);
        owner.outgoing.clear();
        owner.tick(deadline + 1).unwrap();
        assert_eq!(owner.outgoing.len(), 1);
        owner
            .apply_map(br#"{"PeersRemoved":[2]}"#, clock(deadline + 2))
            .unwrap();
        owner.tick(deadline + 10_000).unwrap();
        assert!(owner.retry_series.is_empty());
        assert!(owner.outgoing.is_empty());
    }
    #[test]
    fn automatic_handshake_retries_observe_each_randomized_deadline() {
        let local = DeviceIdentity::generate().unwrap();
        let peer = DeviceIdentity::generate().unwrap();
        let key = peer.node_key().public();
        let mut owner = network(&local, &peer, 1, 2);
        let mut now = 0;
        assert!(!owner.ensure_session(key, now).unwrap());
        for _ in 0..12 {
            let deadline = owner.handshake_retry[&key];
            assert!((5000..=5333).contains(&(deadline - now)));
            owner.outgoing.clear(); // The initiation was lost on the underlay.
            assert!(!owner.ensure_session(key, deadline - 1).unwrap());
            assert!(owner.outgoing.is_empty());
            assert!(!owner.ensure_session(key, deadline).unwrap());
            assert_eq!(owner.handshakes.pending_count(), 1);
            assert_eq!(owner.outgoing.len(), 1);
            now = deadline;
        }
    }
    #[test]
    #[cfg(all(unix, not(target_os = "espidf")))]
    fn host_echo_handles_backpressure_and_revocation() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:1234".parse().unwrap();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        let socket = na.bind_udp(1234, 0).unwrap();
        let target = "100.64.0.2:1234".parse().unwrap();
        let mut echo = crate::host_network::UdpEcho::new(1234).unwrap();
        assert_eq!(echo.poll(&mut nb, 0).unwrap().queued, 0);
        na.initiate(bk, 0).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 1);
        deliver(&mut nb, &mut na, bk, eb, 2);
        deliver(&mut na, &mut nb, ak, ea, 3);
        for value in 0..8 {
            na.send_udp(socket, bk, target, &[value], 4).unwrap();
            deliver(&mut na, &mut nb, ak, ea, 4);
        }
        for _ in 0..OUTGOING {
            nb.advertise(ak, &[eb], 4).unwrap();
        }
        let stats = echo.poll(&mut nb, 4).unwrap();
        assert_eq!((stats.queued, stats.dropped), (0, 8));
        for _ in 0..OUTGOING {
            assert!(nb.flush_one(4, |_, _, _| Ok(())).unwrap());
        }
        na.send_udp(socket, bk, target, b"echo payload", 5).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 5);
        assert_eq!(echo.poll(&mut nb, 5).unwrap().queued, 1);
        deliver(&mut nb, &mut na, bk, eb, 5);
        let response = na.receive_udp(socket, 5).unwrap().unwrap();
        assert_eq!(response.source, target);
        assert_eq!(response.payload, b"echo payload");
        na.send_udp(socket, bk, target, b"revoked", 6).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 6);
        nb.apply_map(br#"{"PacketFilter":[]}"#, clock(7)).unwrap();
        assert_eq!(echo.poll(&mut nb, 7).unwrap().queued, 0);
        echo.close(&mut nb).unwrap();
        nb.disconnect();
        assert_eq!(echo.poll(&mut nb, 8).unwrap().queued, 0);
    }
    #[test]
    fn jailed_peer_udp_requests_round_trip_but_unsolicited_and_closed_socket_replies_fail() {
        fn rejected(
            from: &mut Network<Store>,
            to: &mut Network<Store>,
            source: SocketAddr,
            now: u64,
        ) {
            let mut packet = None;
            assert!(
                from.flush_one(now, |_, _, bytes| {
                    packet = Some(bytes.to_vec());
                    Ok(())
                })
                .unwrap()
            );
            let result = to.receive(Source::Udp(source), &packet.unwrap(), now);
            assert!(
                matches!(
                    result,
                    Err(Error::Handshake(crate::handshake::Error::Plane(
                        PlaneError::Unauthorized
                    )))
                ),
                "unexpected receive result: {result:?}"
            );
        }
        for port in [1234, 49152] {
            let a = DeviceIdentity::generate().unwrap();
            let b = DeviceIdentity::generate().unwrap();
            let ak = a.node_key().public();
            let bk = b.node_key().public();
            let ea = "192.0.2.1:1234".parse().unwrap();
            let eb = "192.0.2.2:1234".parse().unwrap();
            let mut na = network(&a, &b, 1, 2);
            let mut nb = network(&b, &a, 2, 1);
            jail_remote(&mut na, &b);
            let sa = na.bind_udp(port, 0).unwrap();
            let sb = nb.bind_udp(1234, 0).unwrap();
            let da = std::net::SocketAddrV4::new("100.64.0.1".parse().unwrap(), port);
            let db = "100.64.0.2:1234".parse().unwrap();
            na.initiate(bk, 1).unwrap();
            deliver(&mut na, &mut nb, ak, ea, 1);
            deliver(&mut nb, &mut na, bk, eb, 2);
            deliver(&mut na, &mut nb, ak, ea, 3);
            nb.send_udp(sb, ak, da, b"unsolicited", 4).unwrap();
            rejected(&mut nb, &mut na, eb, 4);
            na.send_udp(sa, bk, db, b"request", 5).unwrap();
            deliver(&mut na, &mut nb, ak, ea, 5);
            assert_eq!(nb.receive_udp(sb, 5).unwrap().unwrap().payload, b"request");
            na.apply_map(b"{}", clock(6)).unwrap();
            nb.send_udp(sb, ak, da, b"reply", 6).unwrap();
            deliver(&mut nb, &mut na, bk, eb, 6);
            assert_eq!(na.receive_udp(sa, 6).unwrap().unwrap().payload, b"reply");
            nb.send_udp(sb, ak, da, b"queued reply", 7).unwrap();
            deliver(&mut nb, &mut na, bk, eb, 7);
            na.close_udp(sa).unwrap();
            let reopened = na.bind_udp(port, 7).unwrap();
            assert!(na.receive_udp(reopened, 7).unwrap().is_none());
            nb.send_udp(sb, ak, da, b"late reply", 8).unwrap();
            rejected(&mut nb, &mut na, eb, 8);
            for _ in 0..OUTGOING {
                na.send_udp(reopened, bk, db, b"fill", 9).unwrap();
            }
            let fresh = "100.64.0.2:9000".parse().unwrap();
            assert!(na.send_udp(reopened, bk, fresh, b"not queued", 9).is_err());
            let unsolicited = crate::udp::encode(fresh, da, b"not a reply").unwrap();
            assert!(
                na.coordinator
                    .data_plane()
                    .authenticated_packet(bk, unsolicited, 9)
                    .is_err()
            );
        }
    }

    #[test]
    fn application_udp_round_trip_close_and_policy_revocation() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:1234".parse().unwrap();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        let sa = na.bind_udp(1234, 0).unwrap();
        let sb = nb.bind_udp(1234, 0).unwrap();
        let da = "100.64.0.1:1234".parse().unwrap();
        let db = "100.64.0.2:1234".parse().unwrap();
        assert!(na.send_udp(sa, bk, db, b"no session", 0).is_err());
        na.initiate(bk, 0).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 1);
        deliver(&mut nb, &mut na, bk, eb, 2);
        deliver(&mut na, &mut nb, ak, ea, 3);
        na.send_udp(sa, bk, db, b"request", 4).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 5);
        let packet = nb.receive_udp(sb, 5).unwrap().unwrap();
        assert_eq!(packet.peer, ak);
        assert_eq!(packet.source, da);
        assert_eq!(packet.payload, b"request");
        nb.send_udp(sb, ak, da, b"response", 6).unwrap();
        deliver(&mut nb, &mut na, bk, eb, 7);
        assert_eq!(na.receive_udp(sa, 7).unwrap().unwrap().payload, b"response");
        na.send_udp(sa, bk, db, b"old binding", 7).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 7);
        nb.close_udp(sb).unwrap();
        let sb = nb.bind_udp(1234, 7).unwrap();
        assert!(nb.receive_udp(sb, 7).unwrap().is_none());
        na.send_udp(sa, bk, db, b"queued", 8).unwrap();
        na.close_udp(sa).unwrap();
        assert!(
            !na.flush_one(8, |_, _, _| panic!("closed socket sent"))
                .unwrap()
        );
        let replacement = na.bind_udp(1234, 8).unwrap();
        assert!(na.send_udp(sa, bk, db, b"stale handle", 8).is_err());
        na.send_udp(replacement, bk, db, b"revoke before release", 9)
            .unwrap();
        deliver(&mut na, &mut nb, ak, ea, 10);
        nb.apply_map(br#"{"PacketFilter":[]}"#, clock(11)).unwrap();
        assert!(nb.receive_udp(sb, 11).unwrap().is_none());
        assert_eq!(nb.udp_port(sb, 11).unwrap(), 1234);
        nb.disconnect();
        assert!(nb.receive_udp(sb, 12).is_err());
    }
    #[test]
    fn stun_dispatch_tracks_authenticated_directory_and_disconnect() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let mut owner = network(&a, &b, 1, 2);
        owner.apply_map(br#"{"DERPMap":{"Regions":{"7":{"RegionID":7,"Nodes":[{"Name":"7a","RegionID":7,"HostName":"relay.example","IPv4":"192.0.2.10","STUNPort":3478}]}}}}"#, clock(1)).unwrap();
        let server: SocketAddr = "192.0.2.10:3478".parse().unwrap();
        let mut request = Vec::new();
        assert!(
            owner
                .send_stun(1, |target, bytes| {
                    assert_eq!(target, server);
                    request = bytes.to_vec();
                    Ok(())
                })
                .unwrap()
        );
        let mut response = vec![1, 1, 0, 12, 0x21, 0x12, 0xa4, 0x42];
        response.extend_from_slice(&request[8..20]);
        response.extend_from_slice(&[0, 32, 0, 8, 0, 1]);
        response.extend_from_slice(&(40000u16 ^ 0x2112).to_be_bytes());
        for (byte, mask) in [203, 0, 113, 9].into_iter().zip([0x21, 0x12, 0xa4, 0x42]) {
            response.push(byte ^ mask);
        }
        assert!(
            owner
                .receive(Source::Relay(b.node_key().public()), &response, 2)
                .is_err()
        );
        assert!(
            owner
                .receive(
                    Source::Udp("192.0.2.11:3478".parse().unwrap()),
                    &response,
                    2
                )
                .is_err()
        );
        owner.receive(Source::Udp(server), &response, 3).unwrap();
        assert_eq!(
            owner.observed_endpoints(3).unwrap(),
            vec![
                "203.0.113.9:40000"
                    .parse::<std::net::SocketAddrV4>()
                    .unwrap()
            ]
        );
        assert!(owner.receive_ip(3).unwrap().is_none());
        owner.apply_map(br#"{"KeepAlive":true}"#, clock(4)).unwrap();
        assert_eq!(owner.observed_endpoints(4).unwrap().len(), 1);
        owner
            .apply_map(br#"{"DERPMap":{"Regions":{}}}"#, clock(5))
            .unwrap();
        assert!(owner.observed_endpoints(5).unwrap().is_empty());
        assert!(
            !owner
                .send_stun(5, |_, _| panic!("removed STUN server used"))
                .unwrap()
        );
        owner.disconnect();
        assert!(owner.receive(Source::Udp(server), &response, 6).is_err());
        assert!(owner.observed_endpoints(6).unwrap().is_empty());
    }
    #[test]
    fn home_relay_is_the_region_whose_stun_server_answers_fastest() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let mut owner = network(&a, &b, 1, 2);
        owner.apply_map(br#"{"DERPMap":{"Regions":{"1":{"RegionID":1,"Nodes":[{"Name":"1a","RegionID":1,"HostName":"far.example","IPv4":"192.0.2.1","STUNPort":3478}]},"4":{"RegionID":4,"Nodes":[{"Name":"4a","RegionID":4,"HostName":"near.example","IPv4":"192.0.2.4","STUNPort":3478}]},"9":{"RegionID":9,"Nodes":[{"Name":"9a","RegionID":9,"HostName":"silent.example","IPv4":"192.0.2.9","STUNPort":3478}]}}}}"#, clock(1)).unwrap();
        let answer = |request: &[u8]| {
            let mut response = vec![1, 1, 0, 12, 0x21, 0x12, 0xa4, 0x42];
            response.extend_from_slice(&request[8..20]);
            response.extend_from_slice(&[0, 32, 0, 8, 0, 1]);
            response.extend_from_slice(&(40000u16 ^ 0x2112).to_be_bytes());
            for (byte, mask) in [203, 0, 113, 9].into_iter().zip([0x21, 0x12, 0xa4, 0x42]) {
                response.push(byte ^ mask);
            }
            response
        };
        let mut sent = Vec::new();
        for now in [1, 51, 101] {
            // No home is chosen before the measurements are in.
            assert!(owner.home_relay_plan(now).unwrap().is_none());
            assert!(
                owner
                    .send_stun(now, |target, bytes| {
                        sent.push((target, bytes.to_vec()));
                        Ok(())
                    })
                    .unwrap()
            );
        }
        let reply = |owner: &mut Network<Store>, address: &str, now| {
            let server: SocketAddr = address.parse().unwrap();
            let (_, request) = sent.iter().find(|(target, _)| *target == server).unwrap();
            owner
                .receive(Source::Udp(server), &answer(request), now)
                .unwrap();
        };
        // Region 4 answers after 70 ms, region 1 after 190 ms, region 9 never.
        reply(&mut owner, "192.0.2.4:3478", 121);
        reply(&mut owner, "192.0.2.1:3478", 191);
        assert!(owner.home_relay_plan(400).unwrap().is_none());
        assert_eq!(owner.home_relay_plan(401).unwrap().unwrap().region(), 4);
        // A nearest region that is refused yields the next nearest, then the rest.
        let mut refused = vec![4];
        let next = owner
            .home_relay_matching(401, |plan| !refused.contains(&plan.region()))
            .unwrap()
            .unwrap();
        assert_eq!(next.region(), 1);
        refused.push(1);
        let last = owner
            .home_relay_matching(401, |plan| !refused.contains(&plan.region()))
            .unwrap()
            .unwrap();
        assert_eq!(last.region(), 9);
    }
    #[test]
    fn automatic_probes_cycle_peers_and_recheck_revocation() {
        let local = DeviceIdentity::generate().unwrap();
        let peers: Vec<_> = (0..3)
            .map(|_| DeviceIdentity::generate().unwrap())
            .collect();
        let keys: Vec<_> = peers.iter().map(|p| p.node_key().public()).collect();
        let mut owner =
            Network::new(&local, TimestampClock::open(Store, 100).unwrap(), 4, 2).unwrap();
        let records: Vec<_> = peers
            .iter()
            .enumerate()
            .map(|(index, peer)| {
                json!({
                    "ID":index + 2,
                    "Key":peer.node_key().public().to_string(),
                    "DiscoKey":peer.disco_key().public().to_string(),
                    "Addresses":[format!("100.64.0.{}/32",index+2)],
                    "Endpoints":[format!("192.0.2.{}:1234",index+2)],
                    "MachineAuthorized":true
                })
            })
            .collect();
        let map = json!({"Node":{"ID":1,"Key":local.node_key().public().to_string(),"Addresses":["100.64.0.1/32"],"MachineAuthorized":true},"Peers":records,"PacketFilter":[]});
        owner
            .apply_map(&serde_json::to_vec(&map).unwrap(), clock(0))
            .unwrap();
        let mut sent = Vec::new();
        for (index, now) in [0, 50, 100].into_iter().enumerate() {
            assert!(owner.schedule_probe(now).unwrap());
            assert!(!owner.schedule_probe(now + 1).unwrap());
            assert!(
                owner
                    .flush_one(now + 1, |route, peer, bytes| {
                        assert_eq!(
                            route,
                            Route::Direct(format!("192.0.2.{}:1234", index + 2).parse().unwrap())
                        );
                        assert!(bytes.starts_with(b"TS\xf0\x9f\x92\xac"));
                        sent.push(peer);
                        Ok(())
                    })
                    .unwrap()
            );
        }
        assert_eq!(sent, keys);
        // No duplicate probes while the original transactions are pending.
        for now in (150..5000).step_by(5) {
            assert!(!owner.schedule_probe(now).unwrap());
        }
        // After transaction expiry, the scheduler can retry. Revocation before
        // socket delivery must remove this queued retry as well.
        let mut retry = false;
        for now in (5000..5100).step_by(5) {
            if owner.schedule_probe(now).unwrap() {
                retry = true;
                break;
            }
        }
        assert!(retry);
        owner
            .apply_map(br#"{"PeersRemoved":[2,3,4]}"#, clock(5100))
            .unwrap();
        assert!(!owner.schedule_probe(5100).unwrap());
        assert!(
            !owner
                .flush_one(5100, |_, _, _| panic!("revoked probe sent"))
                .unwrap()
        );
        owner.disconnect();
        assert!(!owner.schedule_probe(5200).unwrap());
        let mut ambiguous = map;
        ambiguous["Peers"][1]["DiscoKey"] = ambiguous["Peers"][0]["DiscoKey"].clone();
        owner
            .apply_map(&serde_json::to_vec(&ambiguous).unwrap(), clock(5300))
            .unwrap();
        assert!(!owner.schedule_probe(5300).unwrap());
        assert!(!owner.schedule_probe(5305).unwrap());
        assert!(owner.schedule_probe(5310).unwrap());
        assert!(
            owner
                .flush_one(5310, |_, peer, _| {
                    assert_eq!(peer, keys[2]);
                    Ok(())
                })
                .unwrap()
        );
    }
    #[test]
    fn owner_connects_relay_direct_fallback_and_queued_revocation() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let ea: SocketAddr = "192.0.2.1:1234".parse().unwrap();
        let eb: SocketAddr = "192.0.2.2:1234".parse().unwrap();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        assert!(na.pending_relay_plans(0).unwrap().is_empty());
        na.initiate(bk, 0).unwrap();
        assert_eq!(na.pending_relay_plans(0).unwrap()[0].region(), 7);
        assert_eq!(deliver(&mut na, &mut nb, ak, ea, 1), Route::Relay(7));
        deliver(&mut nb, &mut na, bk, eb, 2);
        deliver(&mut na, &mut nb, ak, ea, 3);
        na.send_ip(bk, &udp(), 4).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 5);
        let (peer, bytes) = nb.receive_ip(5).unwrap().unwrap();
        assert_eq!(peer, ak);
        assert_eq!(bytes, udp());
        assert!(na.schedule_probe(6).unwrap());
        assert!(na.pending_relay_plans(6).unwrap().is_empty());
        assert_eq!(deliver(&mut na, &mut nb, ak, ea, 7), Route::Direct(eb));
        assert_eq!(deliver(&mut nb, &mut na, bk, eb, 8), Route::Direct(ea));
        na.send_ip(bk, &udp(), 9).unwrap();
        assert!(na.pending_relay_plans(9).unwrap().is_empty());
        assert!(
            na.flush_one(9, |route, _, _| {
                assert_eq!(route, Route::Direct(eb));
                Err(io::ErrorKind::ConnectionRefused.into())
            })
            .is_err()
        );
        assert_eq!(na.pending_relay_plans(10).unwrap()[0].region(), 7);
        assert_eq!(deliver(&mut na, &mut nb, ak, ea, 10), Route::Relay(7));
        nb.apply_map(br#"{"PacketFilter":[]}"#, clock(11)).unwrap();
        assert!(nb.receive_ip(11).unwrap().is_none());
        na.send_ip(bk, &udp(), 12).unwrap();
        na.apply_map(br#"{"PeersRemoved":[2]}"#, clock(13)).unwrap();
        assert!(na.pending_relay_plans(13).unwrap().is_empty());
        assert!(
            !na.flush_one(13, |_, _, _| panic!("revoked output sent"))
                .unwrap()
        );
        assert!(na.send_ip(bk, &udp(), 13).is_err());
    }
    #[test]
    fn failed_discovery_probe_waits_for_fresh_scheduled_attempt() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let bk = b.node_key().public();
        let mut n = network(&a, &b, 1, 2);
        assert!(n.probe(bk, 0).unwrap());
        let mut first = Vec::new();
        assert!(
            !n.flush_one(0, |_, _, bytes| {
                first = bytes.to_vec();
                Err(io::ErrorKind::WouldBlock.into())
            })
            .unwrap()
        );
        assert_eq!(n.outgoing.len(), 1);
        assert!(
            n.flush_one(1, |_, _, bytes| {
                assert_eq!(bytes, first);
                Err(io::ErrorKind::NetworkUnreachable.into())
            })
            .is_err()
        );
        assert!(
            n.outgoing.is_empty(),
            "failed probe retained for immediate retry"
        );
        for now in 2..100 {
            assert!(
                !n.flush_one(now, |_, _, _| panic!("failed probe retried"))
                    .unwrap()
            );
            assert!(!n.probe(bk, now).unwrap());
        }
        assert!(!n.probe(bk, 4999).unwrap());
        assert!(n.probe(bk, 5000).unwrap());
        assert!(
            n.flush_one(5000, |_, _, bytes| {
                assert_ne!(bytes, first, "fresh probe must use a new transaction");
                Ok(())
            })
            .unwrap()
        );
    }

    #[test]
    fn bounded_queues_blocked_sends_expiry_and_clock_rollback() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let bk = b.node_key().public();
        let mut n = network(&a, &b, 1, 2);
        let endpoints = ["192.0.2.1:1234".parse().unwrap()];
        for _ in 0..OUTGOING {
            n.advertise(bk, &endpoints, 0).unwrap();
        }
        assert!(!n.schedule_probe(0).unwrap());
        assert!(matches!(
            n.advertise(bk, &endpoints, 0),
            Err(Error::Capacity)
        ));
        assert!(
            !n.flush_one(1, |_, _, _| Err(io::ErrorKind::WouldBlock.into()))
                .unwrap()
        );
        assert_eq!(n.outgoing.len(), OUTGOING);
        assert!(
            !n.flush_one(5000, |_, _, _| panic!("expired output sent"))
                .unwrap()
        );
        n.advertise(bk, &endpoints, 5000).unwrap();
        assert!(n.tick(4999).is_err());
        assert!(n.outgoing.is_empty());
        assert_eq!(n.local_address(5000).unwrap(), None);
    }
    #[test]
    fn incoming_plaintext_queue_is_bounded_and_drains_through_policy() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:1234".parse().unwrap();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        na.initiate(bk, 0).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 1);
        deliver(&mut nb, &mut na, bk, eb, 2);
        deliver(&mut na, &mut nb, ak, ea, 3);
        for _ in 0..=INCOMING {
            na.send_ip(bk, &udp(), 4).unwrap();
            let mut wire = None;
            na.flush_one(4, |_, _, bytes| {
                wire = Some(bytes.to_vec());
                Ok(())
            })
            .unwrap();
            let result = nb.receive(Source::Relay(ak), &wire.unwrap(), 4);
            // Nothing has bound the port, so a full queue sheds its oldest packet.
            result.unwrap();
        }
        assert_eq!(nb.incoming.len(), INCOMING);
        for _ in 0..INCOMING {
            assert_eq!(nb.receive_ip(5).unwrap().unwrap().1, udp());
        }
        assert!(nb.receive_ip(5).unwrap().is_none());
    }
    #[test]
    fn authenticated_stream_lifetime_revokes_owner_queues() {
        use crate::{map_session::MapSession, protocol::h2::StreamEvent};
        for ending in 0..3 {
            let a = DeviceIdentity::generate().unwrap();
            let b = DeviceIdentity::generate().unwrap();
            let bk = b.node_key().public();
            let mut n = network(&a, &b, 1, 2);
            n.advertise(bk, &["192.0.2.1:1234".parse().unwrap()], 0)
                .unwrap();
            {
                let mut stream = MapSession::new(&mut n);
                assert_eq!(stream.target().local_address(0).unwrap(), None);
                assert!(
                    !stream
                        .target()
                        .flush_one(0, |_, _, _| panic!("old queued output survived new stream"))
                        .unwrap()
                );
                stream
                    .apply(
                        StreamEvent::Headers {
                            status: 200,
                            headers: vec![(":status".into(), "200".into())],
                            end_stream: false,
                        },
                        clock(0),
                    )
                    .unwrap();
                let map=serde_json::to_vec(&json!({"Node":{"ID":1,"Key":a.node_key().public().to_string(),"Addresses":["100.64.0.1/32"],"MachineAuthorized":true},"Peers":[{"ID":2,"Key":bk.to_string(),"DiscoKey":b.disco_key().public().to_string(),"Addresses":["100.64.0.2/32"],"Endpoints":["192.0.2.2:1234"],"MachineAuthorized":true}],"PacketFilter":[]})).unwrap();
                let mut body = (map.len() as u32).to_le_bytes().to_vec();
                body.extend_from_slice(&map);
                for chunk in body.chunks(7) {
                    stream
                        .apply(
                            StreamEvent::Data {
                                payload: chunk.to_vec(),
                                end_stream: false,
                            },
                            clock(0),
                        )
                        .unwrap();
                }
                assert!(stream.target().probe(bk, 1).unwrap());
                stream.target().initiate(bk, 1).unwrap();
                assert_eq!(stream.target().outgoing.len(), 2);
                assert_eq!(stream.target().handshakes.pending_count(), 1);
                match ending {
                    0 => {
                        stream
                            .apply(
                                StreamEvent::Data {
                                    payload: vec![],
                                    end_stream: true,
                                },
                                clock(2),
                            )
                            .unwrap();
                    }
                    1 => {
                        assert!(
                            stream
                                .apply(
                                    StreamEvent::Data {
                                        payload: vec![1, 0, 0, 0, b'!'],
                                        end_stream: false
                                    },
                                    clock(2)
                                )
                                .is_err()
                        );
                    }
                    _ => (),
                }
            }
            assert_eq!(n.local_address(2).unwrap(), None);
            assert!(
                !n.flush_one(2, |_, _, _| panic!("closed stream output sent"))
                    .unwrap()
            );
            assert_eq!(n.handshakes.pending_count(), 0);
        }
    }
    #[test]
    fn tcp_owner_encrypted_transfer_and_revocation() {
        for jailed in [false, true] {
            let a = DeviceIdentity::generate().unwrap();
            let b = DeviceIdentity::generate().unwrap();
            let ak = a.node_key().public();
            let bk = b.node_key().public();
            let ea = "192.0.2.1:1234".parse().unwrap();
            let eb = "192.0.2.2:1234".parse().unwrap();
            let mut na = network(&a, &b, 1, 2);
            let mut nb = network(&b, &a, 2, 1);
            if jailed {
                jail_remote(&mut na, &b);
            }
            let allow = br#"{"PacketFilter":[{"SrcIPs":["*"],"DstPorts":[{"IP":"*","Ports":{"First":80,"Last":80}}],"IPProto":[6]}]}"#;
            na.apply_map(br#"{"PacketFilter":[]}"#, clock(0)).unwrap();
            nb.apply_map(allow, clock(0)).unwrap();
            let target = "100.64.0.2:80".parse().unwrap();
            let socket = na.connect_tcp(bk, target, 0).unwrap();
            let local = na.tcp_local_address(&socket, 0).unwrap();
            let mut server = crate::tcp::Stream::new(target, local, 99, true).unwrap();
            // poll_tcp initiates the authenticated WireGuard session itself.
            na.poll_tcp(&socket, 0).unwrap();
            deliver(&mut na, &mut nb, ak, ea, 1);
            deliver(&mut nb, &mut na, bk, eb, 2);
            deliver(&mut na, &mut nb, ak, ea, 3);
            for now in 4..20 {
                na.poll_tcp(&socket, now).unwrap();
                while !na.outgoing.is_empty() {
                    deliver(&mut na, &mut nb, ak, ea, now);
                }
                while let Some((key, bytes)) = nb.receive_ip(now).unwrap() {
                    assert_eq!(key, ak);
                    server.ingest(&bytes).unwrap();
                }
                server.poll(now).unwrap();
                while let Some(bytes) = server.take_packet() {
                    nb.send_ip(ak, &bytes, now).unwrap();
                }
                while !nb.outgoing.is_empty() {
                    deliver(&mut nb, &mut na, bk, eb, now);
                }
            }
            assert_eq!(
                na.tcp_state(&socket, 20).unwrap(),
                smoltcp::socket::tcp::State::Established
            );
            assert_eq!(na.write_tcp(&socket, b"request", 20).unwrap(), 7);
            na.poll_tcp(&socket, 20).unwrap();
            while !na.outgoing.is_empty() {
                deliver(&mut na, &mut nb, ak, ea, 20);
            }
            while let Some((_, bytes)) = nb.receive_ip(20).unwrap() {
                server.ingest(&bytes).unwrap();
            }
            server.poll(20).unwrap();
            let mut data = [0; 32];
            assert_eq!(server.read(&mut data).unwrap(), 7);
            assert_eq!(&data[..7], b"request");
            server.write(b"response").unwrap();
            server.poll(21).unwrap();
            while let Some(bytes) = server.take_packet() {
                nb.send_ip(ak, &bytes, 21).unwrap();
            }
            while !nb.outgoing.is_empty() {
                deliver(&mut nb, &mut na, bk, eb, 21);
            }
            na.poll_tcp(&socket, 21).unwrap();
            assert_eq!(na.read_tcp(&socket, &mut data[..3], 21).unwrap(), 3);
            assert_eq!(&data[..3], b"res");
            let old_plan = na.relay_plan(bk, 21).unwrap().unwrap();
            for update in [
                br#"{"KeepAlive":true}"#.as_slice(),
                br#"{"PacketFilter":[]}"#,
                br#"{"PeersChangedPatch":[{"NodeID":2,"Endpoints":["192.0.2.99:1234"]}]}"#,
            ] {
                na.apply_map(update, clock(21)).unwrap();
                assert_eq!(
                    na.tcp_state(&socket, 21).unwrap(),
                    smoltcp::socket::tcp::State::Established
                );
            }
            assert!(na.validate_relay_plan(&old_plan, 21).unwrap());
            assert_eq!(na.read_tcp(&socket, &mut data[..1], 21).unwrap(), 1);
            assert_eq!(&data[..1], b"p");
            server.write(b"more").unwrap();
            server.poll(21).unwrap();
            while let Some(bytes) = server.take_packet() {
                nb.send_ip(ak, &bytes, 21).unwrap();
            }
            while !nb.outgoing.is_empty() {
                deliver(&mut nb, &mut na, bk, eb, 21);
            }
            na.poll_tcp(&socket, 21).unwrap();
            na.write_tcp(&socket, b"queued", 21).unwrap();
            na.poll_tcp(&socket, 21).unwrap();
            let permit = retain_delivery(&mut na, 21);
            assert!(na.validate_delivery(&permit, 21).unwrap());
            na.apply_map(allow, clock(22)).unwrap();
            assert!(na.read_tcp(&socket, &mut data, 22).is_err());
            assert!(na.write_tcp(&socket, b"late", 22).is_err());
            assert!(!na.validate_delivery(&permit, 22).unwrap());
            assert!(na.outgoing.is_empty());
            na.apply_map(allow, clock(23)).unwrap();
            let fresh = na.connect_tcp(bk, target, 23).unwrap();
            na.poll_tcp(&fresh, 23).unwrap();
            let pending = retain_delivery(&mut na, 23);
            assert!(na.validate_delivery(&pending, 23).unwrap());
            na.abort_tcp(&fresh).unwrap();
            assert!(!na.validate_delivery(&pending, 23).unwrap());
            assert!(na.outgoing.is_empty());
        }
    }
    #[test]
    fn tcp_connection_survives_renewal_and_retransmits_lost_old_key_data() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let ea = "192.0.2.1:1234".parse().unwrap();
        let eb = "192.0.2.2:1234".parse().unwrap();
        let mut na = network(&a, &b, 1, 2);
        let mut nb = network(&b, &a, 2, 1);
        na.apply_map(br#"{"PacketFilter":[]}"#, clock(0)).unwrap();
        nb.apply_map(br#"{"PacketFilter":[{"SrcIPs":["*"],"DstPorts":[{"IP":"*","Ports":{"First":80,"Last":80}}],"IPProto":[6]}]}"#, clock(0)).unwrap();
        let target = "100.64.0.2:80".parse().unwrap();
        let socket = na.connect_tcp(bk, target, 0).unwrap();
        let local = na.tcp_local_address(&socket, 0).unwrap();
        let mut server = crate::tcp::Stream::new(target, local, 99, true).unwrap();
        na.poll_tcp(&socket, 0).unwrap();
        deliver(&mut na, &mut nb, ak, ea, 1);
        deliver(&mut nb, &mut na, bk, eb, 2);
        deliver(&mut na, &mut nb, ak, ea, 3);
        let original = na
            .coordinator
            .data_plane()
            .session_identity(bk, 3)
            .unwrap()
            .unwrap();
        let mut received = Vec::new();
        let mut echoed = Vec::new();
        let mut expected = Vec::new();
        let mut dropped = 0;
        for now in 4..180103 {
            let payload: &[u8] = match now {
                20 => b"before;",
                120002 => b"during;",
                180003 => b"after;",
                n if n % 10000 == 0 => b".",
                _ => b"",
            };
            if !payload.is_empty() {
                expected.extend_from_slice(payload);
                assert_eq!(na.write_tcp(&socket, payload, now).unwrap(), payload.len());
            }
            na.poll_tcp(&socket, now).unwrap();
            if now == 120002 {
                assert_eq!(na.handshakes.pending_count(), 1);
                // Deliver renewal, then lose the old-key TCP segment in transit.
                deliver(&mut na, &mut nb, ak, ea, now);
                assert!(
                    na.flush_one(now, |_, _, bytes| {
                        assert_eq!(bytes[0], 4);
                        dropped += 1;
                        Ok(())
                    })
                    .unwrap()
                );
            }
            while !na.outgoing.is_empty() {
                deliver(&mut na, &mut nb, ak, ea, now);
            }
            while let Some((key, bytes)) = nb.receive_ip(now).unwrap() {
                assert_eq!(key, ak);
                server.ingest(&bytes).unwrap();
            }
            server.poll(now).unwrap();
            let mut buffer = [0; 64];
            match server.read(&mut buffer) {
                Ok(n) => {
                    received.extend_from_slice(&buffer[..n]);
                    if n != 0 {
                        assert_eq!(server.write(&buffer[..n]).unwrap(), n);
                    }
                }
                Err(error) => assert!(matches!(error, crate::tcp::Error::State)),
            }
            server.poll(now).unwrap();
            while let Some(bytes) = server.take_packet() {
                nb.send_ip(ak, &bytes, now).unwrap();
            }
            while !nb.outgoing.is_empty() {
                deliver(&mut nb, &mut na, bk, eb, now);
            }
            na.poll_tcp(&socket, now).unwrap();
            match na.read_tcp(&socket, &mut buffer, now) {
                Ok(n) => echoed.extend_from_slice(&buffer[..n]),
                Err(error) => assert!(matches!(error, Error::Tcp(crate::tcp::Error::State))),
            }
        }
        assert_eq!(dropped, 1);
        assert_eq!(received, expected);
        assert_eq!(echoed, received);
        assert_eq!(na.tcp_local_address(&socket, 180103).unwrap(), local);
        assert_eq!(
            na.tcp_state(&socket, 180103).unwrap(),
            smoltcp::socket::tcp::State::Established
        );
        let renewed = na
            .coordinator
            .data_plane()
            .session_identity(bk, 180103)
            .unwrap()
            .unwrap();
        assert!(!std::sync::Arc::ptr_eq(&original, &renewed));
    }
    #[test]
    fn tcp_owner_capacity_handles_and_cancellation() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let bk = b.node_key().public();
        let mut na = network(&a, &b, 1, 2);
        let mut other = network(&a, &b, 1, 2);
        let target = "100.64.0.2:80".parse().unwrap();
        let mut sockets = Vec::new();
        for port in 49152..49156 {
            let socket = na.connect_tcp(bk, target, 0).unwrap();
            assert_eq!(na.tcp_local_address(&socket, 0).unwrap().port(), port);
            sockets.push(socket);
        }
        assert!(matches!(
            na.connect_tcp(bk, target, 0),
            Err(Error::Capacity)
        ));
        let foreign = other.connect_tcp(bk, target, 0).unwrap();
        assert!(na.tcp_state(&foreign, 0).is_err());
        na.abort_tcp(&sockets[0]).unwrap();
        assert!(na.tcp_state(&sockets[0], 0).is_err());
        assert!(matches!(
            na.connect_tcp(bk, "100.64.0.9:80".parse().unwrap(), 0),
            Err(Error::Plane(PlaneError::Unauthorized))
        ));
        let fresh = na.connect_tcp(bk, target, 0).unwrap();
        assert_eq!(na.tcp_local_address(&fresh, 0).unwrap().port(), 49156);
        na.apply_map(br#"{"KeepAlive":true}"#, clock(0)).unwrap();
        let local = na.tcp_local_address(&fresh, 0).unwrap();
        let mut client = crate::tcp::Stream::connect(local, target, 1).unwrap();
        let mut server = crate::tcp::Stream::new(target, local, 2, true).unwrap();
        client.poll(0).unwrap();
        server.ingest(&client.take_packet().unwrap()).unwrap();
        server.poll(0).unwrap();
        assert!(
            na.coordinator
                .data_plane()
                .authenticated_packet(bk, server.take_packet().unwrap(), 0)
                .is_err()
        );

        assert!(
            na.connect_tcp(bk, "100.64.0.9:80".parse().unwrap(), 0)
                .is_err()
        );
        na.disconnect();
        assert!(na.tcp_state(&fresh, 0).is_err());
        assert!(na.tcp_connections.is_empty());
    }
    #[test]
    #[cfg(all(unix, not(target_os = "espidf")))]
    fn blocking_tcp_io_transfers_http_until_eof_and_cleans_up() {
        use crate::application_tcp::{Driver, Stream, ThreadShared as Shared};
        use std::{
            io::{Read, Write},
            sync::{Arc, atomic::AtomicBool},
            time::{Duration, Instant},
        };
        struct Harness {
            client: Network<Store>,
            server: Network<Store>,
            ak: Key,
            bk: Key,
            now: u64,
            application: crate::tcp::Stream,
            request: Vec<u8>,
            replied: bool,
            source_port: u16,
            dropped: Arc<AtomicBool>,
            cancel_after_run: Option<Arc<AtomicBool>>,
        }
        impl Driver for Harness {
            type Store = Store;
            fn run<R>(
                &mut self,
                operation: impl FnOnce(&mut Network<Store>, u64) -> Result<R, Error>,
            ) -> io::Result<R> {
                let result = operation(&mut self.client, self.now).map_err(|e| match e {
                    Error::Io(e) => e,
                    e => io::Error::other(e),
                });
                if let Some(cancel) = self.cancel_after_run.take() {
                    cancel.store(true, std::sync::atomic::Ordering::Release);
                }
                result
            }
            fn pump(&mut self) -> io::Result<()> {
                self.now += 1;
                let ea = "192.0.2.1:1234".parse().unwrap();
                let eb = "192.0.2.2:1234".parse().unwrap();
                for _ in 0..16 {
                    if self.client.outgoing.is_empty() {
                        break;
                    }
                    deliver(&mut self.client, &mut self.server, self.ak, ea, self.now);
                }
                while let Some((_, bytes)) = self.server.receive_ip(self.now).unwrap() {
                    let tcp = smoltcp::wire::TcpPacket::new_checked(&bytes[20..]).unwrap();
                    if tcp.syn() && tcp.src_port() != self.source_port {
                        self.source_port = tcp.src_port();
                        self.application = crate::tcp::Stream::new(
                            "100.64.0.2:80".parse().unwrap(),
                            format!("100.64.0.1:{}", self.source_port).parse().unwrap(),
                            99,
                            true,
                        )
                        .unwrap();
                        self.request.clear();
                        self.replied = false;
                    }
                    self.application.ingest(&bytes).unwrap();
                }
                self.application.poll(self.now).unwrap();
                let mut bytes = [0; 256];
                if let Ok(n) = self.application.read(&mut bytes) {
                    self.request.extend_from_slice(&bytes[..n]);
                }
                if !self.replied && self.request.ends_with(b"\r\n\r\n") {
                    assert_eq!(
                        self.request,
                        b"GET / HTTP/1.1\r\nHost: peer.example\r\n\r\n"
                    );
                    self.application
                        .write(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
                        .unwrap();
                    self.application.close();
                    self.replied = true;
                    self.application.poll(self.now).unwrap();
                }
                while let Some(packet) = self.application.take_packet() {
                    self.server.send_ip(self.ak, &packet, self.now).unwrap();
                }
                for _ in 0..16 {
                    if self.server.outgoing.is_empty() {
                        break;
                    }
                    deliver(&mut self.server, &mut self.client, self.bk, eb, self.now);
                }
                Ok(())
            }
        }
        impl Drop for Harness {
            fn drop(&mut self) {
                assert!(self.client.tcp_connections.is_empty());
                self.dropped
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let mut client = network(&a, &b, 1, 2);
        name_peer(&mut client, &b, 0);
        client
            .apply_map(br#"{"PacketFilter":[]}"#, clock(0))
            .unwrap();
        let mut server = network(&b, &a, 2, 1);
        server.apply_map(br#"{"PacketFilter":[{"SrcIPs":["*"],"DstPorts":[{"IP":"*","Ports":{"First":80,"Last":80}}],"IPProto":[6]}]}"#, clock(0)).unwrap();
        let dropped = Arc::new(AtomicBool::new(false));
        let harness = Harness {
            client,
            server,
            ak: a.node_key().public(),
            bk: b.node_key().public(),
            now: 0,
            application: crate::tcp::Stream::new(
                "100.64.0.2:80".parse().unwrap(),
                "100.64.0.1:49152".parse().unwrap(),
                99,
                true,
            )
            .unwrap(),
            request: Vec::new(),
            replied: false,
            source_port: 49152,
            dropped: dropped.clone(),
            cancel_after_run: None,
        };
        let mut shared = Shared::new(harness);
        for _ in 0..2 {
            let worker = shared.clone();
            let dropped = dropped.clone();
            std::thread::spawn(move || {
                let mut shared = worker;
                let cancelled = Arc::new(AtomicBool::new(false));
                let mut stream = Stream::connect(
                    shared.clone(),
                    "peer.example",
                    80,
                    Instant::now() + Duration::from_secs(2),
                    cancelled.clone(),
                )
                .unwrap();
                stream
                    .write_all(b"GET / HTTP/1.1\r\nHost: peer.example\r\n\r\n")
                    .unwrap();
                let mut response = String::new();
                stream.read_to_string(&mut response).unwrap();
                assert_eq!(response, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
                cancelled.store(true, std::sync::atomic::Ordering::Release);
                assert_eq!(
                    stream.read(&mut [0; 1]).unwrap_err().kind(),
                    io::ErrorKind::Interrupted
                );
                drop(stream);
                assert!(!dropped.load(std::sync::atomic::Ordering::Acquire));
                shared
                    .run(|network, _| {
                        assert!(network.tcp_connections.is_empty());
                        Ok(())
                    })
                    .unwrap();
            })
            .join()
            .unwrap();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let mut stream = Stream::connect(
            shared.clone(),
            "peer.example",
            80,
            Instant::now() + Duration::from_secs(2),
            cancel.clone(),
        )
        .unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: peer.example\r\n\r\n")
            .unwrap();
        let mut first = [0];
        stream.read_exact(&mut first).unwrap();
        assert_eq!(first, [b'H']);
        shared
            .with(|driver| {
                driver.cancel_after_run = Some(cancel);
                Ok(())
            })
            .unwrap();
        let mut output = [0xa5; 8];
        assert_eq!(
            stream.read(&mut output).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(output, [0xa5; 8]);
        drop(stream);
        let mut nested = shared.clone();
        let sibling = shared
            .run(|network, now| {
                let first = network.connect_tcp(
                    b.node_key().public(),
                    "100.64.0.2:80".parse().unwrap(),
                    now,
                )?;
                let sibling = network.connect_tcp(
                    b.node_key().public(),
                    "100.64.0.2:80".parse().unwrap(),
                    now,
                )?;
                nested.abort(first).unwrap();
                assert_eq!(network.tcp_connections.len(), 2);
                Ok(sibling)
            })
            .unwrap();
        shared
            .run(|network, now| {
                assert_eq!(network.tcp_connections.len(), 1);
                assert!(network.tcp_state(&sibling, now).is_ok());
                Ok(())
            })
            .unwrap();
        shared.abort(sibling.clone()).unwrap();
        shared.abort(sibling).unwrap(); // Already removed: cleanup remains idempotent.
        drop(nested);
        drop(shared);
        assert!(dropped.load(std::sync::atomic::Ordering::Acquire));
    }
    #[test]
    #[cfg(all(unix, not(target_os = "espidf")))]
    fn blocking_tcp_io_cleans_up_stalled_handshakes() {
        use crate::host_tcp::{Driver, Stream};
        use std::{
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
            time::{Duration, Instant},
        };
        struct Waiting {
            network: Network<Store>,
            cancelled: Arc<AtomicBool>,
            mode: u8,
            now: u64,
            dropped: Arc<AtomicBool>,
        }
        impl Driver for Waiting {
            type Store = Store;
            fn run<R>(
                &mut self,
                operation: impl FnOnce(&mut Network<Store>, u64) -> Result<R, Error>,
            ) -> io::Result<R> {
                operation(&mut self.network, self.now).map_err(|e| match e {
                    Error::Io(e) => e,
                    e => io::Error::other(e),
                })
            }
            fn pump(&mut self) -> io::Result<()> {
                self.now += 1;
                assert_eq!(self.network.tcp_connections.len(), 1);
                match self.mode {
                    0 => self.cancelled.store(true, Ordering::Release),
                    1 => (),
                    _ => return Err(io::ErrorKind::ConnectionAborted.into()),
                }
                Ok(())
            }
        }
        impl Drop for Waiting {
            fn drop(&mut self) {
                assert!(self.network.tcp_connections.is_empty());
                self.dropped.store(true, Ordering::Release);
            }
        }
        for mode in 0..3 {
            let a = DeviceIdentity::generate().unwrap();
            let b = DeviceIdentity::generate().unwrap();
            let mut network = network(&a, &b, 1, 2);
            name_peer(&mut network, &b, 0);
            let cancelled = Arc::new(AtomicBool::new(false));
            let dropped = Arc::new(AtomicBool::new(false));
            let driver = Waiting {
                network,
                cancelled: cancelled.clone(),
                mode,
                now: 0,
                dropped: dropped.clone(),
            };
            let timeout = if mode == 1 {
                Duration::from_millis(20)
            } else {
                Duration::from_secs(2)
            };
            let error = match Stream::connect(
                driver,
                "peer.example",
                80,
                Instant::now() + timeout,
                cancelled,
            ) {
                Ok(_) => panic!("stalled connection succeeded"),
                Err(error) => error,
            };
            assert_eq!(
                error.kind(),
                [
                    io::ErrorKind::Interrupted,
                    io::ErrorKind::TimedOut,
                    io::ErrorKind::ConnectionAborted
                ][usize::from(mode)]
            );
            assert!(dropped.load(Ordering::Acquire));
        }
    }
    #[test]
    #[cfg(all(unix, not(target_os = "espidf")))]
    fn tailnet_tls_verifies_certificates_eof_and_buffered_plaintext_authority() {
        use crate::host_tcp::{Driver, Shared};
        use crate::host_tcp_tls::Connector;
        use std::{
            io::{Read, Write},
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
            time::{Duration, Instant},
        };
        struct Harness {
            client: Network<Store>,
            server: Network<Store>,
            ak: Key,
            bk: Key,
            now: u64,
            tcp: crate::tcp::Stream,
            tls: rustls::ServerConnection,
            replied: bool,
            request: Vec<u8>,
            truncate: bool,
        }
        impl Driver for Harness {
            type Store = Store;
            fn run<R>(
                &mut self,
                operation: impl FnOnce(&mut Network<Store>, u64) -> Result<R, Error>,
            ) -> io::Result<R> {
                operation(&mut self.client, self.now).map_err(|e| match e {
                    Error::Io(e) => e,
                    e => io::Error::other(e),
                })
            }
            fn pump(&mut self) -> io::Result<()> {
                self.now += 1;
                for _ in 0..16 {
                    if self.client.outgoing.is_empty() {
                        break;
                    }
                    deliver(
                        &mut self.client,
                        &mut self.server,
                        self.ak,
                        "192.0.2.1:1234".parse().unwrap(),
                        self.now,
                    );
                }
                while let Some((_, bytes)) = self.server.receive_ip(self.now).unwrap() {
                    self.tcp.ingest(&bytes).unwrap();
                }
                self.tcp.poll(self.now).unwrap();
                let mut bytes = [0; 4096];
                if let Ok(n) = self.tcp.read(&mut bytes)
                    && n > 0
                {
                    self.tls.read_tls(&mut &bytes[..n]).unwrap();
                    self.tls.process_new_packets().unwrap();
                }
                if let Ok(n) = self.tls.reader().read(&mut bytes) {
                    self.request.extend_from_slice(&bytes[..n]);
                }
                if !self.replied && self.request == b"request" {
                    self.tls.writer().write_all(b"private reply").unwrap();
                    if !self.truncate {
                        self.tls.send_close_notify();
                    }
                    self.replied = true;
                }
                while self.tls.wants_write() {
                    let mut encrypted = Vec::new();
                    self.tls.write_tls(&mut encrypted).unwrap();
                    assert_eq!(self.tcp.write(&encrypted).unwrap(), encrypted.len());
                }
                if self.replied {
                    self.tcp.close();
                }
                self.tcp.poll(self.now).unwrap();
                while let Some(packet) = self.tcp.take_packet() {
                    self.server.send_ip(self.ak, &packet, self.now).unwrap();
                }
                for _ in 0..16 {
                    if self.server.outgoing.is_empty() {
                        break;
                    }
                    deliver(
                        &mut self.server,
                        &mut self.client,
                        self.bk,
                        "192.0.2.2:1234".parse().unwrap(),
                        self.now,
                    );
                }
                Ok(())
            }
        }
        impl Drop for Harness {
            fn drop(&mut self) {
                assert!(self.client.tcp_connections.is_empty());
            }
        }
        // Valid, wrong name, expired, untrusted, cancelled buffered read, revoked buffered read, truncated TLS.
        for mode in 0..8 {
            let key = rcgen::KeyPair::generate().unwrap();
            let name = if mode == 1 {
                "wrong.example"
            } else {
                "peer.example"
            };
            let mut params = rcgen::CertificateParams::new(vec![name.into()]).unwrap();
            if mode == 2 {
                params.not_before = rcgen::date_time_ymd(2020, 1, 1);
                params.not_after = rcgen::date_time_ymd(2021, 1, 1);
            }
            let cert = params.self_signed(&key).unwrap();
            let mut roots = rustls::RootCertStore::empty();
            if mode != 3 {
                roots.add(cert.der().clone()).unwrap();
            }
            let config = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
            )
            .unwrap();
            let a = DeviceIdentity::generate().unwrap();
            let b = DeviceIdentity::generate().unwrap();
            let mut client = network(&a, &b, 1, 2);
            name_peer(&mut client, &b, 0);
            client
                .apply_map(br#"{"PacketFilter":[]}"#, clock(0))
                .unwrap();
            let mut server = network(&b, &a, 2, 1);
            server.apply_map(br#"{"PacketFilter":[{"SrcIPs":["*"],"DstPorts":[{"IP":"*","Ports":{"First":443,"Last":443}}],"IPProto":[6]}]}"#, clock(0)).unwrap();
            let mut shared = Shared::new(Harness {
                client,
                server,
                ak: a.node_key().public(),
                bk: b.node_key().public(),
                now: 0,
                tcp: crate::tcp::Stream::new(
                    "100.64.0.2:443".parse().unwrap(),
                    "100.64.0.1:49152".parse().unwrap(),
                    99,
                    true,
                )
                .unwrap(),
                tls: rustls::ServerConnection::new(Arc::new(config)).unwrap(),
                replied: false,
                request: Vec::new(),
                truncate: mode == 6,
            });
            let cancelled = Arc::new(AtomicBool::new(false));
            let result = Connector::with_roots(roots).unwrap().connect(
                shared.clone(),
                "peer.example",
                443,
                Instant::now() + Duration::from_secs(3),
                cancelled.clone(),
            );
            if (1..=3).contains(&mode) {
                assert!(result.is_err(), "certificate mode {mode}");
            } else {
                let mut stream = result.unwrap();
                stream.write_all(b"request").unwrap();
                stream.flush().unwrap();
                if mode == 7 {
                    let mut prefix = [0; 1];
                    stream.read_exact(&mut prefix).unwrap();
                    shared
                        .run(|network, now| network.apply_map(br#"{"KeepAlive":true}"#, clock(now)))
                        .unwrap();
                    let mut rest = Vec::new();
                    stream.read_to_end(&mut rest).unwrap();
                    assert_eq!(&prefix, b"p");
                    assert_eq!(rest, b"rivate reply");
                } else if mode == 4 || mode == 5 {
                    let mut prefix = [0; 1];
                    stream.read_exact(&mut prefix).unwrap();
                    assert_eq!(&prefix, b"p");
                    if mode == 4 {
                        cancelled.store(true, Ordering::Release);
                    } else {
                        shared
                            .run(|network, now| {
                                network.apply_map(br#"{"PeersRemoved":[2]}"#, clock(now))
                            })
                            .unwrap();
                    }
                    let mut unread = [0xAA; 32];
                    assert!(stream.read(&mut unread).is_err());
                    assert_eq!(unread, [0xAA; 32]);
                } else {
                    let mut response = Vec::new();
                    let result = stream.read_to_end(&mut response);
                    if mode == 6 {
                        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
                    } else {
                        result.unwrap();
                    }
                    assert_eq!(response, b"private reply");
                }
                drop(stream);
            }
            shared
                .run(|network, _| {
                    assert!(network.tcp_connections.is_empty());
                    Ok(())
                })
                .unwrap();
        }
    }
}
