//! Coordinator JSON translation. Inputs must already be authenticated.
use crate::{
    control_stream::MAX_MAP_BYTES,
    plane::{DataPlane, Peer, PlaneError},
    protocol::key::{Disco, Node, PublicKey},
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, Ipv4Addr, SocketAddr},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
type Key = PublicKey<Node>;

#[derive(Debug, thiserror::Error)]
pub enum MapError {
    #[error("invalid coordinator JSON")]
    Json(#[from] serde_json::Error),
    #[error("invalid or unsupported coordinator node update: {0}")]
    Node(&'static str),
    #[error("coordinator capacity exceeded")]
    Capacity,
    #[error("invalid or backwards clock")]
    Clock,
    #[error(transparent)]
    Plane(#[from] PlaneError),
    #[error(transparent)]
    Relay(#[from] crate::relay::RelayError),
}

/// Wall time must come from a trusted platform clock. Both readings describe
/// the same instant; Unix milliseconds are signed to reject pre-epoch clocks.
#[derive(Clone, Copy)]
pub struct Clock {
    pub unix_ms: i64,
    pub monotonic_ms: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WireNode {
    #[serde(rename = "ID")]
    id: u64,
    key: Key,
    addresses: Vec<String>,
    #[serde(rename = "AllowedIPs")]
    allowed_ips: Option<Vec<String>>,
    key_expiry: Option<String>,
    #[serde(default)]
    expired: bool,
    #[serde(default)]
    machine_authorized: bool,
    #[serde(default)]
    is_jailed: bool,
    #[serde(rename = "UnsignedPeerAPIOnly", default)]
    unsigned_peer_api_only: bool,
    #[serde(default)]
    name: String,
    disco_key: Option<PublicKey<Disco>>,
    endpoints: Option<Vec<String>>,
    #[serde(rename = "HomeDERP", default)]
    home_derp: u64,
    #[serde(rename = "DERP", default)]
    legacy_derp: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Patch {
    #[serde(rename = "NodeID")]
    id: u64,
    key: Option<Key>,
    key_expiry: Option<String>,
    disco_key: Option<PublicKey<Disco>>,
    endpoints: Option<Vec<String>>,
    #[serde(rename = "DERPRegion", default)]
    derp_region: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Map {
    node: Option<WireNode>,
    peers: Option<Vec<WireNode>>,
    peers_changed: Option<Vec<WireNode>>,
    peers_removed: Option<Vec<u64>>,
    peers_changed_patch: Option<Vec<Patch>>,
    #[serde(rename = "DERPMap")]
    relay_map: Option<crate::relay::RelayUpdate>,
}
#[derive(Deserialize)]
struct MapHeader {
    #[serde(rename = "KeepAlive", default)]
    keep_alive: bool,
}
/// Borrowed through the coordinator after current authorization checks. These
/// are coordinator hints, not proof of endpoint reachability or peer identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerRoute {
    pub name: Option<String>,
    pub discovery_key: Option<PublicKey<Disco>>,
    pub endpoints: Vec<SocketAddr>,
    pub relay_region: Option<u64>,
}

#[derive(Clone)]
struct Record {
    peer: Peer,
    permitted: bool,
    route: PeerRoute,
}

pub struct Coordinator {
    plane: DataPlane,
    local_key: Key,
    local_address: Ipv4Addr,
    discover_address: bool,
    local_deadline: Option<u64>,
    nodes: BTreeMap<u64, Record>,
    max_peers: usize,
    max_sessions: usize,
    synchronized: bool,
    last_clock: Option<Clock>,
    last_runtime_ms: u64,
    relays: crate::relay::RelayDirectory,
    relay_identity: std::sync::Arc<()>,
}

impl Coordinator {
    /// Processes one authenticated response body and invalidates all data-plane
    /// authorization when it ends, fails, or is cancelled. The body reader must
    /// enforce read deadlines. Reconnect begins a fresh full synchronization.
    pub fn read_stream<R: std::io::Read>(
        &mut self,
        reader: &mut R,
        cancelled: impl FnMut() -> bool,
        mut clock: impl FnMut() -> Clock,
    ) -> Result<(), crate::control_stream::ReadError<MapError>> {
        let result =
            crate::control_stream::read_maps(reader, cancelled, |bytes| self.apply(bytes, clock()));
        self.disconnect();
        result
    }
    pub fn new(
        local_key: Key,
        local_address: Ipv4Addr,
        max_peers: usize,
        max_sessions: usize,
        now_ms: u64,
    ) -> Result<Self, MapError> {
        if local_key == Key::zero() {
            return Err(MapError::Node("zero local key"));
        }
        Ok(Self {
            plane: DataPlane::new(local_address, max_peers, max_sessions, now_ms)?,
            local_key,
            local_address,
            discover_address: false,
            local_deadline: None,
            nodes: BTreeMap::new(),
            max_peers,
            max_sessions,
            synchronized: false,
            last_clock: None,
            last_runtime_ms: now_ms,
            relays: Default::default(),
            relay_identity: std::sync::Arc::new(()),
        })
    }
    /// Learns one assigned IPv4 address from the authenticated initial map.
    /// No packet authorization exists before that complete map is committed.
    pub fn discovering(
        local_key: Key,
        max_peers: usize,
        max_sessions: usize,
        now_ms: u64,
    ) -> Result<Self, MapError> {
        let mut coordinator = Self::new(
            local_key,
            Ipv4Addr::UNSPECIFIED,
            max_peers,
            max_sessions,
            now_ms,
        )?;
        coordinator.discover_address = true;
        Ok(coordinator)
    }
    /// Returns the assigned address only while local authorization is current.
    pub fn local_address(&mut self, now_ms: u64) -> Result<Option<Ipv4Addr>, MapError> {
        self.runtime_clock(now_ms)?;
        if !self.synchronized {
            return Ok(None);
        }
        if self
            .local_deadline
            .is_none_or(|deadline| deadline <= now_ms)
        {
            self.disconnect();
            return Ok(None);
        }
        Ok(Some(self.local_address))
    }
    /// Runtime packet and handshake operations use this same plane. Do not call
    /// its apply_update or disconnect directly; the coordinator owns map state.
    pub fn data_plane(&mut self) -> &mut DataPlane {
        &mut self.plane
    }
    fn runtime_clock(&mut self, now_ms: u64) -> Result<(), MapError> {
        if now_ms < self.last_runtime_ms {
            self.disconnect();
            return Err(MapError::Clock);
        }
        self.last_runtime_ms = now_ms;
        Ok(())
    }
    /// Discovery keys must identify exactly one currently authorized peer.
    pub fn peer_for_discovery(
        &mut self,
        discovery_key: PublicKey<Disco>,
        now_ms: u64,
    ) -> Result<Option<Key>, MapError> {
        self.runtime_clock(now_ms)?;
        let mut found = None;
        for node in self.nodes.values() {
            if node.route.discovery_key == Some(discovery_key)
                && self.plane.begin_handshake(node.peer.key, now_ms).is_ok()
            {
                if found.is_some() {
                    return Err(MapError::Node("ambiguous discovery key"));
                }
                found = Some(node.peer.key);
            }
        }
        Ok(found)
    }
    /// Exact FQDN matching, ignoring ASCII case and one trailing dot. No search
    /// suffixes, external DNS, or ambiguous short-name expansion are performed.
    pub fn resolve(
        &mut self,
        name: &str,
        now_ms: u64,
    ) -> Result<Option<(Key, Ipv4Addr)>, MapError> {
        self.runtime_clock(now_ms)?;
        let Some(name) = dns_name(name)? else {
            return Ok(None);
        };
        let mut matches = self.nodes.values().filter(|node| {
            node.route.name.as_ref() == Some(&name)
                && node.permitted
                && !node.peer.addresses.is_empty()
                && node.peer.expires_ms > now_ms
        });
        let Some(node) = matches.next() else {
            return Ok(None);
        };
        if matches.next().is_some() {
            return Err(MapError::Node("ambiguous peer"));
        }
        let answer = (node.peer.key, node.peer.addresses[0]);
        if self.plane.begin_handshake(answer.0, now_ms).is_err() {
            return Ok(None);
        }
        Ok(Some(answer))
    }
    pub fn route(&mut self, key: Key, now_ms: u64) -> Result<Option<&PeerRoute>, MapError> {
        self.runtime_clock(now_ms)?;
        if self.plane.begin_handshake(key, now_ms).is_err() {
            return Ok(None);
        }
        Ok(self
            .nodes
            .values()
            .find(|node| node.peer.key == key)
            .map(|node| &node.route))
    }
    /// Returns the next authorized discovery peer in node-ID order. The cursor
    /// is only a traversal hint; each result checks current authorization.
    pub(crate) fn discovery_peer_after(
        &mut self,
        after: Option<u64>,
        now_ms: u64,
    ) -> Result<Option<(u64, Key)>, MapError> {
        use std::ops::Bound::{Excluded, Unbounded};
        if self.local_address(now_ms)?.is_none() {
            return Ok(None);
        }
        for (&id, node) in self
            .nodes
            .range((after.map_or(Unbounded, Excluded), Unbounded))
        {
            if node.route.discovery_key.is_some()
                && self.plane.begin_handshake(node.peer.key, now_ms).is_ok()
            {
                return Ok(Some((id, node.peer.key)));
            }
        }
        Ok(None)
    }
    /// Resolves the authorized peer's current home region. Server order is
    /// preserved; STUN-only nodes must not be used for relay connections.
    pub fn relay_for(
        &mut self,
        key: Key,
        now_ms: u64,
    ) -> Result<Option<&crate::relay::Region>, MapError> {
        let region = self
            .route(key, now_ms)?
            .and_then(|route| route.relay_region);
        Ok(region.and_then(|id| self.relays.region(id)))
    }
    pub(crate) fn home_relay_matching(
        &mut self,
        now_ms: u64,
        mut accept: impl FnMut(u64, &crate::relay::RelayNode) -> bool,
    ) -> Result<Option<(u64, crate::relay::RelayNode)>, MapError> {
        if self.local_address(now_ms)?.is_none() {
            return Ok(None);
        }
        for id in self.relays.region_ids() {
            let region = self
                .relays
                .region(id)
                .ok_or(MapError::Node("unknown relay region"))?;
            if region.no_measure_no_home {
                continue;
            }
            if let Some(node) = region
                .nodes
                .iter()
                .find(|node| node.can_relay() && accept(id, node))
            {
                return Ok(Some((id, node.clone())));
            }
        }
        Ok(None)
    }
    pub(crate) fn contains_relay(&self, id: u64, node: &crate::relay::RelayNode) -> bool {
        self.relays
            .region(id)
            .is_some_and(|region| region.nodes.contains(node))
    }
    pub(crate) fn relay_identity(&self) -> std::sync::Arc<()> {
        self.relay_identity.clone()
    }
    pub(crate) fn relay_region(&self, id: u64) -> Option<&crate::relay::Region> {
        self.relays.region(id)
    }
    pub(crate) fn allows_home_relay(&self, id: u64, node: &crate::relay::RelayNode) -> bool {
        node.can_relay()
            && self
                .relays
                .region(id)
                .is_some_and(|region| !region.no_measure_no_home && region.nodes.contains(node))
    }
    pub fn disconnect(&mut self) {
        self.plane.disconnect();
        self.nodes.clear();
        self.relays = Default::default();
        self.relay_identity = std::sync::Arc::new(());
        self.local_deadline = None;
        self.synchronized = false;
    }
    pub(crate) fn stun_servers(
        &mut self,
        now_ms: u64,
    ) -> Result<Vec<std::net::SocketAddrV4>, MapError> {
        let mut servers = Vec::new();
        if self.local_address(now_ms)?.is_none() {
            return Ok(servers);
        }
        for id in self.relays.region_ids() {
            for node in &self
                .relays
                .region(id)
                .ok_or(MapError::Node("unknown relay region"))?
                .nodes
            {
                if let (crate::relay::Address::Fixed(ip), Some(port)) = (&node.ipv4, node.stun_port)
                {
                    let endpoint = std::net::SocketAddrV4::new(*ip, port);
                    if !servers.contains(&endpoint) {
                        servers.push(endpoint);
                    }
                    if servers.len() == 8 {
                        return Ok(servers);
                    }
                }
            }
        }
        Ok(servers)
    }
    /// Each map commits peer state and policy together. Any error disconnects
    /// authorization and requires a fresh stream with a full map and local node.
    pub fn apply(&mut self, bytes: &[u8], clock: Clock) -> Result<(), MapError> {
        self.apply_event(bytes, clock).map(|_| ())
    }
    /// Returns whether the event committed a map rather than a heartbeat.
    pub(crate) fn apply_event(&mut self, bytes: &[u8], clock: Clock) -> Result<bool, MapError> {
        let result = self.apply_inner(bytes, clock);
        if result.is_err() {
            self.disconnect();
        }
        result
    }
    fn apply_inner(&mut self, bytes: &[u8], clock: Clock) -> Result<bool, MapError> {
        self.runtime_clock(clock.monotonic_ms)?;
        if bytes.len() > MAX_MAP_BYTES {
            return Err(MapError::Capacity);
        }
        if clock.unix_ms < 0
            || self.last_clock.is_some_and(|old| {
                clock.monotonic_ms < old.monotonic_ms || clock.unix_ms < old.unix_ms
            })
        {
            return Err(MapError::Clock);
        }
        if let Some(old) = self.last_clock {
            let wall_elapsed = i128::from(clock.unix_ms) - i128::from(old.unix_ms);
            let monotonic_elapsed = i128::from(clock.monotonic_ms - old.monotonic_ms);
            if (wall_elapsed - monotonic_elapsed).abs() > 1000 {
                self.last_clock = Some(clock);
                return Err(MapError::Clock);
            }
        }
        self.last_clock = Some(clock);
        let header: MapHeader = serde_json::from_slice(bytes)?;
        if header.keep_alive {
            // The control protocol excludes peer, policy and relay updates from heartbeats.
            if self.synchronized
                && self
                    .local_deadline
                    .is_none_or(|deadline| deadline <= clock.monotonic_ms)
            {
                return Err(MapError::Node("heartbeat after local expiry"));
            }
            return Ok(false);
        }
        let map: Map = serde_json::from_slice(bytes)?;
        let initial = !self.synchronized;
        let relays = self.relays.updated(map.relay_map)?;
        let full = initial || map.peers.as_ref().is_some_and(|peers| !peers.is_empty());
        let mut local_address = self.local_address;
        let local_deadline = match map.node {
            Some(node) => {
                let record = record(node, clock)?;
                if record.peer.key != self.local_key || !record.permitted || record.peer.jailed {
                    return Err(MapError::Node(
                        "local node not this device or not authorized",
                    ));
                }
                if initial && self.discover_address {
                    let [address] = record.peer.addresses.as_slice() else {
                        return Err(MapError::Node("local node needs one IPv4 address"));
                    };
                    if address.is_unspecified()
                        || address.is_loopback()
                        || address.is_multicast()
                        || address.is_broadcast()
                    {
                        return Err(MapError::Node("unusable local address"));
                    }
                    local_address = *address;
                } else if !record.peer.addresses.contains(&local_address) {
                    // Address changes require a fresh stream and new data plane.
                    return Err(MapError::Node("local address changed"));
                }
                record.peer.expires_ms
            }
            None => self.local_deadline.ok_or(MapError::Node("no local node"))?,
        };
        if local_deadline <= clock.monotonic_ms {
            return Err(MapError::Node("local node expired"));
        }
        let changed = map.peers_changed.unwrap_or_default();
        let removed = map.peers_removed.unwrap_or_default();
        let patches = map.peers_changed_patch.unwrap_or_default();
        if changed.len() > self.max_peers
            || removed.len() > self.max_peers
            || patches.len() > self.max_peers
        {
            return Err(MapError::Capacity);
        }
        let mut nodes = if full {
            BTreeMap::new()
        } else {
            self.nodes.clone()
        };
        let mut touched = BTreeSet::new();
        if let Some(peers) = map.peers.filter(|peers| !peers.is_empty()) {
            if peers.len() > self.max_peers {
                return Err(MapError::Capacity);
            }
            for node in peers {
                if !touched.insert(node.id) {
                    return Err(MapError::Node("duplicate peer"));
                }
                nodes.insert(node.id, record(node, clock)?);
            }
        } else {
            // Tailscale's first response lists peers as changes to an empty map.
            for id in removed {
                if id == 0 || !touched.insert(id) {
                    return Err(MapError::Node("invalid removal"));
                }
                nodes.remove(&id);
            }
            for node in changed {
                if !touched.insert(node.id) {
                    return Err(MapError::Node("duplicate changed peer"));
                }
                nodes.insert(node.id, record(node, clock)?);
            }
        }
        let mut patched = BTreeSet::new();
        for patch in patches {
            if !patched.insert(patch.id) {
                return Err(MapError::Node("duplicate patch"));
            }
            let Some(node) = nodes.get_mut(&patch.id) else {
                continue;
            };
            if let Some(key) = patch.key {
                node.peer.key = key;
            }
            if let Some(discovery_key) = patch.disco_key {
                node.route.discovery_key = nonzero_discovery(discovery_key);
            }
            if let Some(endpoints) = patch.endpoints.filter(|endpoints| !endpoints.is_empty()) {
                node.route.endpoints = parse_endpoints(endpoints)?;
            }
            if patch.derp_region > crate::relay::MAX_REGION_ID {
                return Err(MapError::Node("patch relay region"));
            }
            if patch.derp_region != 0 {
                node.route.relay_region = Some(patch.derp_region);
            }
            if let Some(expiry) = patch.key_expiry {
                node.peer.expires_ms = deadline(Some(&expiry), clock)?;
            }
        }
        if nodes.len() > self.max_peers {
            return Err(MapError::Capacity);
        }
        let mut keys = BTreeSet::new();
        let mut peers = Vec::new();
        for node in nodes.values() {
            if node.peer.key == Key::zero()
                || node.peer.key == self.local_key
                || !keys.insert(node.peer.key)
            {
                return Err(MapError::Node("zero, local or duplicate peer key"));
            }
            if node.permitted && !node.peer.addresses.is_empty() {
                let mut peer = node.peer.clone();
                peer.expires_ms = peer.expires_ms.min(local_deadline);
                peers.push(peer);
            }
        }
        let new_keys: BTreeSet<_> = peers.iter().map(|p| p.key).collect();
        let removals: Vec<_> = if initial {
            Vec::new()
        } else {
            self.nodes
                .values()
                .map(|n| n.peer.key)
                .filter(|k| !new_keys.contains(k))
                .collect()
        };
        if initial && self.discover_address {
            let mut plane = DataPlane::new(
                local_address,
                self.max_peers,
                self.max_sessions,
                clock.monotonic_ms,
            )?;
            plane.apply_update(true, &peers, &removals, bytes, clock.monotonic_ms)?;
            self.plane = plane;
        } else {
            self.plane
                .apply_update(initial, &peers, &removals, bytes, clock.monotonic_ms)?;
        }
        self.local_address = local_address;
        self.nodes = nodes;
        if self.relays != relays {
            self.relay_identity = std::sync::Arc::new(());
        }
        self.relays = relays;
        self.local_deadline = Some(local_deadline);
        self.synchronized = true;
        Ok(true)
    }
}

fn record(node: WireNode, clock: Clock) -> Result<Record, MapError> {
    if node.id == 0 || node.key == Key::zero() || node.addresses.len() > 8 {
        return Err(MapError::Node("node id, key or address count"));
    }
    let mut addresses = Vec::new();
    for prefix in &node.addresses {
        let (ip, bits) = prefix
            .rsplit_once('/')
            .ok_or(MapError::Node("address prefix"))?;
        let ip: IpAddr = ip.parse().map_err(|_| MapError::Node("address"))?;
        let bits: u8 = bits.parse().map_err(|_| MapError::Node("prefix length"))?;
        match ip {
            IpAddr::V4(ip) if bits == 32 => {
                if addresses.contains(&ip) {
                    return Err(MapError::Node("duplicate address"));
                }
                addresses.push(ip);
            }
            IpAddr::V6(_) if bits == 128 => (),
            _ => return Err(MapError::Node("address prefix length")),
        }
    }
    if addresses.len() > 4 {
        return Err(MapError::Capacity);
    }
    if let Some(allowed) = node.allowed_ips {
        if allowed.len() > 256 {
            return Err(MapError::Capacity);
        }
        // Subnet/exit routing is outside the prototype. Only exact assigned
        // addresses explicitly present in AllowedIPs are installed.
        addresses.retain(|ip| allowed.contains(&format!("{ip}/32")));
    }
    let expires_ms = if node.expired {
        clock.monotonic_ms
    } else {
        deadline(node.key_expiry.as_deref(), clock)?
    };
    if node.home_derp > crate::relay::MAX_REGION_ID {
        return Err(MapError::Node("home relay region"));
    }
    let relay_region = if node.home_derp != 0 {
        Some(node.home_derp)
    } else if node.legacy_derp.is_empty() {
        None
    } else {
        let endpoint: SocketAddr = node
            .legacy_derp
            .parse()
            .map_err(|_| MapError::Node("legacy relay"))?;
        if endpoint.ip() != IpAddr::V4(Ipv4Addr::new(127, 3, 3, 40)) || endpoint.port() == 0 {
            return Err(MapError::Node("legacy relay"));
        }
        Some(u64::from(endpoint.port()))
    };
    let route = PeerRoute {
        name: dns_name(&node.name)?,
        discovery_key: node.disco_key.and_then(nonzero_discovery),
        endpoints: parse_endpoints(node.endpoints.unwrap_or_default())?,
        relay_region,
    };
    Ok(Record {
        route,
        peer: Peer {
            key: node.key,
            addresses,
            expires_ms,
            jailed: node.is_jailed,
        },
        permitted: node.machine_authorized && !node.unsigned_peer_api_only && !node.expired,
    })
}
fn nonzero_discovery(key: PublicKey<Disco>) -> Option<PublicKey<Disco>> {
    (key != PublicKey::zero()).then_some(key)
}
pub(crate) fn dns_name(name: &str) -> Result<Option<String>, MapError> {
    if name.is_empty() {
        return Ok(None);
    }
    let name = name.strip_suffix('.').unwrap_or(name);
    if name.len() > 253
        || name.is_empty()
        || !name.is_ascii()
        || name.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return Err(MapError::Node("peer name"));
    }
    Ok(Some(name.to_ascii_lowercase()))
}
fn parse_endpoints(values: Vec<String>) -> Result<Vec<SocketAddr>, MapError> {
    if values.len() > 16 {
        return Err(MapError::Capacity);
    }
    let mut endpoints = Vec::new();
    for value in values {
        if value.len() > 80 {
            return Err(MapError::Node("endpoint length"));
        }
        let endpoint: SocketAddr = value.parse().map_err(|_| MapError::Node("endpoint"))?;
        let ip = endpoint.ip().to_canonical();
        if endpoint.port() == 0
            || ip.is_unspecified()
            || ip.is_multicast()
            || ip.is_loopback()
            || matches!(ip, IpAddr::V4(ip) if ip.is_broadcast())
        {
            return Err(MapError::Node("unusable endpoint"));
        }
        if !endpoints.contains(&endpoint) {
            endpoints.push(endpoint);
        }
    }
    Ok(endpoints)
}

fn deadline(value: Option<&str>, clock: Clock) -> Result<u64, MapError> {
    let Some(value) = value else {
        return Ok(u64::MAX);
    };
    if value.len() > 40 {
        return Err(MapError::Clock);
    }
    let instant = OffsetDateTime::parse(value, &Rfc3339).map_err(|_| MapError::Clock)?;
    if instant.unix_timestamp_nanos() == -62_135_596_800_i128 * 1_000_000_000 {
        return Ok(u64::MAX);
    }
    let remaining = instant.unix_timestamp_nanos() / 1_000_000 - i128::from(clock.unix_ms);
    if remaining <= 0 {
        return Ok(clock.monotonic_ms);
    }
    clock
        .monotonic_ms
        .checked_add(u64::try_from(remaining).map_err(|_| MapError::Clock)?)
        .ok_or(MapError::Clock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn key(n: u8) -> Key {
        Key::from_bytes([n; 32])
    }
    fn clock(n: u64) -> Clock {
        Clock {
            unix_ms: 1_735_689_600_000 + n as i64,
            monotonic_ms: n,
        }
    }
    fn node(id: u64, n: u8) -> Value {
        json!({"ID":id,"Key":key(n).to_string(),"Addresses":[format!("100.64.0.{n}/32"),"fd7a:115c:a1e0::1/128"],"MachineAuthorized":true})
    }
    fn full() -> Value {
        json!({"Node":node(1,1), "Peers":[node(2,2)], "PacketFilter":[]})
    }
    fn coordinator() -> Coordinator {
        Coordinator::new(key(1), Ipv4Addr::new(100, 64, 0, 1), 4, 2, 0).unwrap()
    }
    fn apply(c: &mut Coordinator, value: Value, now: u64) -> Result<(), MapError> {
        c.apply(&serde_json::to_vec(&value).unwrap(), clock(now))
    }
    fn authorized(c: &mut Coordinator, n: u8, now: u64) -> bool {
        c.data_plane().begin_handshake(key(n), now).is_ok()
    }

    #[test]
    fn jailed_peer_cannot_initiate_tcp_despite_allow_filter() {
        let mut c = coordinator();
        let mut map = full();
        map["Peers"][0]["IsJailed"] = json!(true);
        map["PacketFilter"] = json!([{"SrcIPs":["100.64.0.2"],
            "DstPorts":[{"IP":"100.64.0.1","Ports":{"First":1234,"Last":1234}}],"IPProto":[6]}]);
        apply(&mut c, map, 0).unwrap();
        assert!(authorized(&mut c, 2, 1));
        let mut remote = crate::tcp::Stream::connect(
            "100.64.0.2:49152".parse().unwrap(),
            "100.64.0.1:1234".parse().unwrap(),
            1,
        )
        .unwrap();
        remote.poll(0).unwrap();
        let syn = remote.take_packet().unwrap();
        assert!(matches!(
            c.data_plane().authenticated_packet(key(2), syn, 1),
            Err(PlaneError::Unauthorized)
        ));
    }

    #[test]
    fn assigned_address_discovery_and_reconnection() {
        let mut c = Coordinator::discovering(key(1), 4, 2, 0).unwrap();
        assert_eq!(c.local_address(0).unwrap(), None);
        assert!(!authorized(&mut c, 2, 0));
        apply(&mut c, json!({"KeepAlive":true}), 0).unwrap();
        assert_eq!(c.local_address(0).unwrap(), None);
        apply(&mut c, full(), 0).unwrap();
        assert_eq!(
            c.local_address(0).unwrap(),
            Some(Ipv4Addr::new(100, 64, 0, 1))
        );
        assert!(authorized(&mut c, 2, 0));
        let mut changed = full();
        changed["Node"]["Addresses"] = json!(["100.64.0.9/32"]);
        assert!(apply(&mut c, changed.clone(), 1).is_err());
        assert_eq!(c.local_address(1).unwrap(), None);
        assert!(!authorized(&mut c, 2, 1));
        apply(&mut c, changed, 2).unwrap();
        assert_eq!(
            c.local_address(2).unwrap(),
            Some(Ipv4Addr::new(100, 64, 0, 9))
        );
        c.disconnect();
        assert_eq!(c.local_address(2).unwrap(), None);
    }

    #[test]
    fn discovery_requires_one_authorized_address_and_complete_valid_map() {
        let mut invalid = Vec::new();
        for addresses in [
            json!([]),
            json!(["fd7a:115c:a1e0::1/128"]),
            json!(["100.64.0.1/32", "100.64.0.2/32"]),
            json!(["0.0.0.0/32"]),
            json!(["127.0.0.1/32"]),
            json!(["224.0.0.1/32"]),
            json!(["255.255.255.255/32"]),
        ] {
            let mut map = full();
            map["Node"]["Addresses"] = addresses;
            invalid.push(map);
        }
        for (field, value) in [
            ("Key", json!(key(9).to_string())),
            ("MachineAuthorized", json!(false)),
            ("Expired", json!(true)),
            ("AllowedIPs", json!([])),
        ] {
            let mut map = full();
            map["Node"][field] = value;
            invalid.push(map);
        }
        let mut bad_policy = full();
        bad_policy["PacketFilter"] = json!("invalid");
        invalid.push(bad_policy);
        for map in invalid {
            let mut c = Coordinator::discovering(key(1), 4, 2, 0).unwrap();
            assert!(apply(&mut c, map, 0).is_err());
            assert_eq!(c.local_address(0).unwrap(), None);
            assert!(!authorized(&mut c, 2, 0));
        }
    }

    #[test]
    fn discovered_address_is_hidden_after_expiry_and_clock_rollback() {
        let mut c = Coordinator::discovering(key(1), 4, 2, 0).unwrap();
        let mut map = full();
        map["Node"]["KeyExpiry"] = json!("2025-01-01T00:00:00.010Z");
        apply(&mut c, map, 0).unwrap();
        assert!(c.local_address(9).unwrap().is_some());
        assert_eq!(c.local_address(10).unwrap(), None);
        assert!(!authorized(&mut c, 2, 10));
        apply(&mut c, full(), 11).unwrap();
        assert!(c.local_address(10).is_err());
        assert_eq!(c.local_address(11).unwrap(), None);
    }

    #[test]
    fn relay_directory_commits_with_peer_updates_and_clears_on_error() {
        let mut c = coordinator();
        let mut map = full();
        map["Peers"][0]["HomeDERP"] = json!(70000);
        let relay_map = json!({"Regions":{"70000":{"RegionID":70000,"NoMeasureNoHome":true,"Nodes":[
            {"Name":"first","RegionID":70000,"HostName":"first.example","STUNOnly":true},
            {"Name":"second","RegionID":70000,"HostName":"second.example"}
        ]}}});
        map["DERPMap"] = relay_map;
        apply(&mut c, map, 0).unwrap();
        let region = c.relay_for(key(2), 0).unwrap().unwrap();
        assert!(region.no_measure_no_home);
        assert_eq!(region.nodes[0].name, "first");
        assert!(region.nodes[0].stun_only);
        assert_eq!(region.nodes[1].name, "second");
        apply(&mut c, json!({"DERPMap":{"HomeParams":{}}}), 1).unwrap();
        assert!(c.relay_for(key(2), 1).unwrap().is_some());
        apply(&mut c, json!({"DERPMap":{"Regions":{}}}), 2).unwrap();
        assert!(c.relay_for(key(2), 2).unwrap().is_none());
        assert!(authorized(&mut c, 2, 2));
        assert!(
            apply(
                &mut c,
                json!({"DERPMap":{"Regions":{"7":{"RegionID":8,"Nodes":[]}}}}),
                3
            )
            .is_err()
        );
        assert!(!authorized(&mut c, 2, 3));
        assert!(c.relay_for(key(2), 3).unwrap().is_none());
    }

    #[test]
    fn discovery_routes_follow_patches_and_key_rotation() {
        let mut c = coordinator();
        let mut map = full();
        map["Peers"][0]["Name"] = json!("Reader.Tail.Example.");
        map["Peers"][0]["DiscoKey"] = json!(PublicKey::<Disco>::from_bytes([9; 32]).to_string());
        map["Peers"][0]["Endpoints"] = json!([
            "192.168.1.2:41641",
            "192.168.1.2:41641",
            "[2001:db8::2]:41641"
        ]);
        map["Peers"][0]["DERP"] = json!("127.3.3.40:7");
        apply(&mut c, map, 0).unwrap();
        let original = c.route(key(2), 0).unwrap().unwrap().clone();
        assert_eq!(original.name.as_deref(), Some("reader.tail.example"));
        assert_eq!(original.endpoints.len(), 2);
        assert_eq!(original.relay_region, Some(7));
        assert!(original.discovery_key.is_some());
        apply(&mut c, json!({"PeersChangedPatch":[{"NodeID":2,"Endpoints":[],"DERPRegion":0},{"NodeID":99,"Endpoints":["192.168.9.9:12"]}]}),1).unwrap();
        assert_eq!(c.route(key(2), 1).unwrap().unwrap(), &original);
        apply(&mut c,json!({"PeersChangedPatch":[{"NodeID":2,"Key":key(3).to_string(),"DiscoKey":PublicKey::<Disco>::zero().to_string(),"DERPRegion":8,"Endpoints":["192.168.1.3:41641"]}]}),2).unwrap();
        assert!(c.route(key(2), 2).unwrap().is_none());
        let route = c.route(key(3), 2).unwrap().unwrap();
        assert_eq!(route.relay_region, Some(8));
        assert!(route.discovery_key.is_none());
        assert_eq!(
            route.endpoints,
            vec!["192.168.1.3:41641".parse::<SocketAddr>().unwrap()]
        );
        assert_eq!(
            c.resolve("READER.tail.example.", 2).unwrap(),
            Some((key(3), Ipv4Addr::new(100, 64, 0, 2)))
        );
        apply(&mut c, json!({"PeersRemoved":[2]}), 3).unwrap();
        assert!(c.resolve("reader.tail.example", 3).unwrap().is_none());
    }

    #[test]
    fn names_are_exact_and_ambiguous_names_do_not_resolve() {
        let mut c = coordinator();
        let mut map = full();
        map["Peers"][0]["Name"] = json!("reader.tail.example.");
        map["Peers"][0]["KeyExpiry"] = json!("2025-01-01T00:00:01Z");
        apply(&mut c, map, 0).unwrap();
        assert!(c.resolve("reader", 0).unwrap().is_none());
        assert!(c.resolve("reader.tail.example", 999).unwrap().is_some());
        assert!(c.resolve("reader.tail.example", 1000).unwrap().is_none());
        assert!(c.route(key(2), 1000).unwrap().is_none());
        assert!(c.resolve("reader.tail.example", 999).is_err());
        assert!(!authorized(&mut c, 2, 1000));
        let mut c = coordinator();
        let mut map = full();
        map["Peers"] = json!([node(2, 2), node(3, 3)]);
        for index in [0, 1] {
            map["Peers"][index]["Name"] = json!("same.tail.example.");
        }
        apply(&mut c, map, 0).unwrap();
        assert!(c.resolve("same.tail.example", 0).is_err());
        assert!(c.route(key(2), 0).unwrap().is_some());
    }

    #[test]
    fn invalid_route_updates_clear_old_routes_and_names() {
        for endpoints in [
            json!(["127.0.0.1:1234"]),
            json!(["[::ffff:127.0.0.1]:1234"]),
            json!(["192.168.1.1:0"]),
            json!(["host.example:1234"]),
            json!(["224.0.0.1:1234"]),
            json!(vec!["192.168.1.1:1234"; 17]),
        ] {
            let mut c = coordinator();
            apply(&mut c, full(), 0).unwrap();
            assert!(
                apply(
                    &mut c,
                    json!({"PeersChangedPatch":[{"NodeID":2,"Endpoints":endpoints}]}),
                    1
                )
                .is_err()
            );
            assert!(c.route(key(2), 1).unwrap().is_none());
        }
        for name in ["bad..example", "-bad.example", "bad_.example", "é.example"] {
            let mut c = coordinator();
            let mut map = full();
            map["Peers"][0]["Name"] = json!(name);
            assert!(apply(&mut c, map, 0).is_err());
        }
    }

    #[test]
    fn first_map_may_list_peers_as_changes() {
        let mut c = coordinator();
        apply(
            &mut c,
            json!({"Node":node(1,1), "PeersChanged":[node(2,2)], "PeersChangedPatch":[{"NodeID":2,"DERPRegion":8}]}),
            0,
        )
        .unwrap();
        assert!(authorized(&mut c, 2, 0));
    }
    #[test]
    fn full_delta_rotation_and_removal_follow_node_ids() {
        let mut c = coordinator();
        apply(&mut c, full(), 0).unwrap();
        assert!(authorized(&mut c, 2, 0));
        apply(&mut c, json!({"PeersChanged":[node(2,3)]}), 1).unwrap();
        assert!(!authorized(&mut c, 2, 1));
        assert!(authorized(&mut c, 3, 1));
        apply(
            &mut c,
            json!({"PeersChangedPatch":[{"NodeID":2,"Key":key(4).to_string()}]}),
            2,
        )
        .unwrap();
        assert!(!authorized(&mut c, 3, 2));
        assert!(authorized(&mut c, 4, 2));
        apply(&mut c, json!({"PeersRemoved":[2]}), 3).unwrap();
        assert!(!authorized(&mut c, 4, 3));
    }
    #[test]
    fn heartbeat_ignores_map_fields_without_authorizing_or_revoking() {
        let heartbeat = json!({"KeepAlive":true,"Node":"ignored","PeersRemoved":[2],
            "PeersChanged":false,"PacketFilter":"ignored","DERPMap":42});
        let mut c = coordinator();
        apply(&mut c, heartbeat.clone(), 0).unwrap();
        assert!(c.local_address(0).unwrap().is_none());
        assert!(!authorized(&mut c, 2, 0));
        apply(&mut c, full(), 1).unwrap();
        let identity = c.data_plane().authorization_identity();
        apply(&mut c, heartbeat, 2).unwrap();
        assert!(authorized(&mut c, 2, 2));
        assert!(std::sync::Arc::ptr_eq(
            &identity,
            &c.data_plane().authorization_identity()
        ));
        apply(&mut c, json!({"PeersRemoved":[2]}), 3).unwrap();
        assert!(!authorized(&mut c, 2, 3));
    }
    #[test]
    fn malformed_heartbeat_clears_authorization() {
        for bytes in [
            br#"{"KeepAlive":true,"KeepAlive":false}"#.as_slice(),
            br#"{"KeepAlive":"true"}"#,
            br#"{"KeepAlive":true,"Node":}"#,
        ] {
            let mut c = coordinator();
            apply(&mut c, full(), 0).unwrap();
            assert!(c.apply(bytes, clock(1)).is_err());
            assert!(!authorized(&mut c, 2, 1));
        }
    }
    #[test]
    fn expiry_patch_and_local_expiry_bound_handshakes() {
        let mut c = coordinator();
        let mut map = full();
        map["Node"]["KeyExpiry"] = json!("2025-01-01T00:00:02Z");
        apply(&mut c, map, 0).unwrap();
        apply(
            &mut c,
            json!({"PeersChangedPatch":[{"NodeID":2,"KeyExpiry":"2025-01-01T00:00:01Z"}]}),
            10,
        )
        .unwrap();
        assert!(authorized(&mut c, 2, 999));
        assert!(!authorized(&mut c, 2, 1000));
        apply(
            &mut c,
            json!({"PeersChangedPatch":[{"NodeID":2,"KeyExpiry":"0001-01-01T00:00:00Z"}]}),
            1000,
        )
        .unwrap();
        assert!(authorized(&mut c, 2, 1999));
        assert!(!authorized(&mut c, 2, 2000));
        assert!(apply(&mut c, json!({"KeepAlive":true}), 2000).is_err());
    }
    #[test]
    fn malformed_or_unsupported_updates_revoke_and_require_full_resync() {
        for bad in [
            json!({"PeersChangedPatch":[{"NodeID":2,"Key":key(3).to_string()},{"NodeID":2,"Key":key(4).to_string()}]}),
            json!({"PeersChanged":[node(3,2)]}),
            json!({"PeersRemoved":[2,2]}),
            json!({"PeersChangedPatch":[{"NodeID":2,"KeyExpiry":"not a time"}]}),
            json!({"PeersChangedPatch":[{"NodeID":2,"Key":key(1).to_string()}]}),
            json!({"PacketFilter":[{"UnsupportedField":true}]}),
        ] {
            let mut c = coordinator();
            apply(&mut c, full(), 0).unwrap();
            assert!(apply(&mut c, bad, 1).is_err());
            assert!(!authorized(&mut c, 2, 1));
            assert!(apply(&mut c, json!({"PeersChanged":[node(2,2)]}), 2).is_err());
            apply(&mut c, full(), 3).unwrap();
            assert!(authorized(&mut c, 2, 3));
        }
    }
    #[test]
    fn withdrawn_authorization_and_routes_do_not_remain_installed() {
        for field in [
            "MachineAuthorized",
            "Expired",
            "UnsignedPeerAPIOnly",
            "AllowedIPs",
        ] {
            let mut c = coordinator();
            apply(&mut c, full(), 0).unwrap();
            let mut changed = node(2, 2);
            changed[field] = match field {
                "MachineAuthorized" => json!(false),
                "AllowedIPs" => json!(["0.0.0.0/0"]),
                _ => json!(true),
            };
            apply(&mut c, json!({"PeersChanged":[changed]}), 1).unwrap();
            assert!(!authorized(&mut c, 2, 1));
        }
    }
    #[test]
    fn duplicate_json_clock_steps_and_stream_end_clear_authorization() {
        let mut c = coordinator();
        apply(&mut c, full(), 0).unwrap();
        assert!(
            c.apply(br#"{"PeersRemoved":[],"PeersRemoved":[2]}"#, clock(1))
                .is_err()
        );
        apply(&mut c, full(), 2).unwrap();
        let jump = Clock {
            unix_ms: clock(3).unix_ms + 10_000,
            monotonic_ms: 3,
        };
        assert!(c.apply(b"{}", jump).is_err());
        assert!(!authorized(&mut c, 2, 3));
        let mut c = coordinator();
        let bytes = serde_json::to_vec(&full()).unwrap();
        let mut wire = (bytes.len() as u32).to_le_bytes().to_vec();
        wire.extend(bytes);
        c.read_stream(&mut std::io::Cursor::new(wire), || false, || clock(0))
            .unwrap();
        assert!(!authorized(&mut c, 2, 0));
    }
    #[test]
    fn jail_updates_revoke_pending_packets_and_malformed_flags_fail_closed() {
        for value in [json!(null), json!("true"), json!(1)] {
            let mut c = coordinator();
            apply(&mut c, full(), 0).unwrap();
            let mut changed = node(2, 2);
            changed["IsJailed"] = value;
            assert!(apply(&mut c, json!({"PeersChanged":[changed]}), 1).is_err());
            assert!(!authorized(&mut c, 2, 1));
        }
        for full_update in [false, true] {
            let mut c = coordinator();
            let mut map = full();
            map["PacketFilter"] = json!([{"SrcIPs":["100.64.0.2"],"DstPorts":[{"IP":"100.64.0.1","Ports":{"First":1234,"Last":1234}}],"IPProto":[17]}]);
            apply(&mut c, map, 0).unwrap();
            let bytes = crate::udp::encode(
                "100.64.0.2:9000".parse().unwrap(),
                "100.64.0.1:1234".parse().unwrap(),
                b"incoming",
            )
            .unwrap();
            let pending = c
                .data_plane()
                .authenticated_packet(key(2), bytes.clone(), 1)
                .unwrap()
                .unwrap();
            let mut peer = node(2, 2);
            peer["IsJailed"] = json!(true);
            let update = if full_update {
                let mut replacement = full();
                replacement["Peers"] = json!([peer]);
                replacement["PacketFilter"] = json!([{"SrcIPs":["100.64.0.2"],"DstPorts":[{"IP":"100.64.0.1","Ports":{"First":1234,"Last":1234}}],"IPProto":[17]}]);
                replacement
            } else {
                json!({"PeersChanged":[peer]})
            };
            apply(&mut c, update, 2).unwrap();
            assert!(authorized(&mut c, 2, 2));
            assert!(c.data_plane().release(pending, 2).is_err());
            assert!(
                c.data_plane()
                    .authenticated_packet(key(2), bytes.clone(), 2)
                    .is_err()
            );
            apply(&mut c, json!({"PeersChanged":[node(2, 2)]}), 3).unwrap();
            assert!(
                c.data_plane()
                    .authenticated_packet(key(2), bytes, 3)
                    .is_ok()
            );
        }
        let mut c = coordinator();
        let mut map = full();
        map["Node"]["IsJailed"] = json!(true);
        assert!(apply(&mut c, map, 0).is_err());
        assert!(!authorized(&mut c, 2, 0));
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::{
        protocol::{
            identity::DeviceIdentity,
            wireguard::{HandshakeInitiator, HandshakeResponder},
        },
        transport::Session,
    };
    use serde_json::json;
    use std::time::{Duration, UNIX_EPOCH};
    #[test]
    fn peer_list_replacement_preserves_omitted_policy_and_delta_revokes_queued_data() {
        let remote = DeviceIdentity::generate().unwrap();
        let local = DeviceIdentity::generate().unwrap();
        let peer_key = remote.node_key().public();
        let mut initiator =
            HandshakeInitiator::new(remote.node_key(), local.node_key().public(), 1).unwrap();
        let initiation = initiator
            .initiation(UNIX_EPOCH + Duration::from_secs(100))
            .unwrap();
        let response = HandshakeResponder::respond(local.node_key(), &initiation, 2).unwrap();
        let mut sender = Session::new(initiator.finish(&response.packet).unwrap(), 0);
        let mut c = Coordinator::new(
            local.node_key().public(),
            Ipv4Addr::new(100, 64, 0, 1),
            4,
            2,
            0,
        )
        .unwrap();
        let peer = json!({"ID":2,"Key":peer_key.to_string(),"Addresses":["100.64.0.2/32"],"MachineAuthorized":true});
        let full = json!({"Node":{"ID":1,"Key":local.node_key().public().to_string(),"Addresses":["100.64.0.1/32"],"MachineAuthorized":true},"Peers":[peer.clone()],"PacketFilter":[{"SrcIPs":["100.64.0.2"],"DstPorts":[{"IP":"100.64.0.1","Ports":{"First":1234,"Last":1234}}],"IPProto":[17]}]});
        let clock = Clock {
            unix_ms: 1_735_689_600_000,
            monotonic_ms: 0,
        };
        c.apply(&serde_json::to_vec(&full).unwrap(), clock).unwrap();
        let permit = c.data_plane().begin_handshake(peer_key, 0).unwrap();
        c.data_plane()
            .install(permit, Session::new(response.session, 0), 0)
            .unwrap();
        // A replacement peer list must not reset an omitted packet filter.
        c.apply(
            &serde_json::to_vec(&json!({"Peers":[peer]})).unwrap(),
            clock,
        )
        .unwrap();
        // An empty Peers array in a subsequent response is not a directory reset.
        c.apply(br#"{"Peers":[]}"#, clock).unwrap();
        let mut udp = vec![0; 28];
        udp[0] = 0x45;
        udp[3] = 28;
        udp[8] = 64;
        udp[9] = 17;
        udp[12..16].copy_from_slice(&[100, 64, 0, 2]);
        udp[16..20].copy_from_slice(&[100, 64, 0, 1]);
        udp[20..22].copy_from_slice(&9000_u16.to_be_bytes());
        udp[22..24].copy_from_slice(&1234_u16.to_be_bytes());
        udp[25] = 8;
        let encrypted = sender.encrypt(&udp, 0).unwrap();
        let queued = c
            .data_plane()
            .receive(peer_key, &encrypted, 0)
            .unwrap()
            .unwrap();
        c.apply(br#"{"PacketFilter":[]}"#, clock).unwrap();
        assert!(c.data_plane().release(queued, 0).is_err());
        let encrypted = sender.encrypt(&udp, 0).unwrap();
        assert!(c.data_plane().receive(peer_key, &encrypted, 0).is_err());
    }
}
