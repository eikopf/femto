//! Property test of inbound packet admission against a reference written
//! from `docs/tailscale-data-plane.md` and `docs/tcp.md`.
//!
//! The reference never parses packet bytes or policy text: it decides from
//! the description each was generated from.
use super::*;
use proptest::prelude::*;
use serde_json::json;
use std::{collections::BTreeMap, net::SocketAddrV4, sync::Arc};

const LOCAL: Ipv4Addr = Ipv4Addr::new(100, 64, 0, 1);
const START: u64 = 2000;

fn ip(last: u8) -> Ipv4Addr {
    Ipv4Addr::new(100, 64, 0, last)
}
/// Peer 0 owns .2 and .3, peer 1 owns .4, peer 2 is not in the directory.
fn key(index: usize) -> Key {
    Key::from_bytes([index as u8 + 1; 32])
}
fn owned(index: usize) -> &'static [u8] {
    match index {
        0 => &[2, 3],
        1 => &[4],
        _ => &[],
    }
}

// ------------------------------------------------------------------- policy

#[derive(Clone, Debug)]
enum Address {
    Star,
    Single(Ipv4Addr),
    Prefix(Ipv4Addr, u8),
    Range(Ipv4Addr, Ipv4Addr),
    V6(&'static str),
}
impl Address {
    fn text(&self) -> String {
        match self {
            Self::Star => "*".into(),
            Self::Single(ip) => ip.to_string(),
            Self::Prefix(ip, bits) => format!("{ip}/{bits}"),
            Self::Range(a, b) => format!("{a}-{b}"),
            Self::V6(text) => (*text).into(),
        }
    }
    fn contains(&self, ip: Ipv4Addr) -> bool {
        let value = u64::from(u32::from(ip));
        match self {
            Self::Star => true,
            Self::Single(other) => *other == ip,
            Self::Prefix(base, bits) => {
                let shift = 32 - u32::from(*bits);
                (value >> shift) == (u64::from(u32::from(*base)) >> shift)
            }
            Self::Range(a, b) => u32::from(*a) <= u32::from(ip) && u32::from(ip) <= u32::from(*b),
            Self::V6(_) => false,
        }
    }
}
#[derive(Clone, Debug)]
struct Rule {
    sources: Vec<Address>,
    destinations: Vec<(Address, u16, u16)>,
    protocols: Option<Vec<u8>>,
}
impl Rule {
    fn json(&self) -> serde_json::Value {
        let mut rule = json!({
            "SrcIPs": self.sources.iter().map(Address::text).collect::<Vec<_>>(),
            "DstPorts": self.destinations.iter().map(|(ip, first, last)|
                json!({"IP": ip.text(), "Ports": {"First": first, "Last": last}})).collect::<Vec<_>>(),
        });
        if let Some(protocols) = &self.protocols {
            rule["IPProto"] = json!(protocols);
        }
        rule
    }
    /// ICMP has no ports; the implementation files it under port zero.
    fn allows(&self, source: Ipv4Addr, destination: Ipv4Addr, protocol: u8, port: u16) -> bool {
        let protocols: &[u8] = match &self.protocols {
            Some(list) if !list.is_empty() => list,
            _ => &[1, 6, 17],
        };
        protocols.contains(&protocol)
            && self.sources.iter().any(|s| s.contains(source))
            && self
                .destinations
                .iter()
                .any(|(d, first, last)| d.contains(destination) && *first <= port && port <= *last)
    }
}
fn address() -> impl Strategy<Value = Address> {
    let host = prop_oneof![Just(0u8), Just(1), Just(2), Just(3), Just(4), Just(5), Just(9)];
    prop_oneof![
        3 => Just(Address::Star),
        3 => host.clone().prop_map(|h| Address::Single(ip(h))),
        4 => (host.clone(), prop_oneof![Just(0u8), Just(1), Just(10), Just(24), Just(29), Just(30), Just(31), Just(32)])
            .prop_map(|(h, bits)| Address::Prefix(ip(h), bits)),
        1 => Just(Address::Prefix(Ipv4Addr::new(0, 0, 0, 0), 0)),
        1 => Just(Address::Prefix(Ipv4Addr::new(255, 255, 255, 255), 1)),
        3 => (host.clone(), host).prop_map(|(a, b)| Address::Range(ip(a.min(b)), ip(a.max(b)))),
        1 => Just(Address::Range(Ipv4Addr::new(0, 0, 0, 0), Ipv4Addr::new(255, 255, 255, 255))),
        1 => prop_oneof![Just("fd7a:115c:a1e0::1"), Just("fd7a:115c:a1e0::/48"), Just("::/0")].prop_map(Address::V6),
    ]
}
fn port() -> impl Strategy<Value = u16> {
    prop_oneof![Just(0u16), Just(1), Just(79), Just(80), Just(81), Just(1234), Just(40000), Just(40001), Just(65535)]
}
fn rule() -> impl Strategy<Value = Rule> {
    (
        prop::collection::vec(address(), 1..3),
        prop::collection::vec((address(), prop_oneof![port(), Just(0u16)], prop_oneof![port(), Just(65535u16)]), 1..3),
        prop::option::of(prop::collection::vec(
            prop_oneof![Just(1u8), Just(6), Just(17), Just(47)],
            0..3,
        )),
    )
        .prop_map(|(sources, destinations, protocols)| Rule {
            sources,
            destinations: destinations
                .into_iter()
                .map(|(a, x, y)| (a, x.min(y), x.max(y)))
                .collect(),
            protocols,
        })
}
fn rules() -> impl Strategy<Value = Vec<Rule>> {
    prop::collection::vec(rule(), 0..3)
}

/// One coordinator message's policy part.
#[derive(Clone, Debug)]
struct Update {
    full: bool,
    base: Option<Vec<Rule>>,
    clear: bool,
    named: Vec<(&'static str, Option<Vec<Rule>>)>,
}
impl Update {
    fn json(&self) -> Vec<u8> {
        let mut map = serde_json::Map::new();
        if let Some(base) = &self.base {
            map.insert("PacketFilter".into(), json!(base.iter().map(Rule::json).collect::<Vec<_>>()));
        }
        let mut named = serde_json::Map::new();
        if self.clear {
            named.insert("*".into(), serde_json::Value::Null);
        }
        for (name, rules) in &self.named {
            named.insert(
                (*name).into(),
                match rules {
                    Some(rules) => json!(rules.iter().map(Rule::json).collect::<Vec<_>>()),
                    None => serde_json::Value::Null,
                },
            );
        }
        if !named.is_empty() {
            map.insert("PacketFilters".into(), named.into());
        }
        serde_json::to_vec(&map).unwrap()
    }
    /// Full updates start from denial; the legacy filter is the group named
    /// "base"; a null "*" drops every earlier group; a null group is removed.
    fn apply(&self, groups: &mut BTreeMap<String, Vec<Rule>>) {
        if self.full {
            groups.clear();
        }
        if let Some(base) = &self.base {
            groups.insert("base".into(), base.clone());
        }
        if self.clear {
            groups.clear();
        }
        // Later duplicates of a name do not occur: the generator names each once.
        for (name, rules) in &self.named {
            match rules {
                Some(rules) => groups.insert((*name).into(), rules.clone()),
                None => groups.remove(*name),
            };
        }
    }
}
fn update(full: bool) -> impl Strategy<Value = Update> {
    (
        prop::option::weighted(if full { 0.8 } else { 0.3 }, rules()),
        prop::bool::weighted(0.15),
        prop::collection::btree_map(
            prop_oneof![Just("a"), Just("b"), Just("base")],
            prop::option::weighted(0.75, rules()),
            0..3,
        ),
    )
        .prop_map(move |(base, clear, named)| Update {
            full,
            base,
            clear,
            named: named.into_iter().collect(),
        })
}

// ------------------------------------------------------------------ packets

#[derive(Clone, Copy, Debug, PartialEq)]
enum Header {
    Plain,
    DontFragment,
    MoreFragments,
    Offset,
    Reserved,
    Options,
    TrailingByte,
    TotalTooLong,
}
#[derive(Clone, Debug)]
enum Transport {
    /// `length` is the whole segment; `offset` the header length in words.
    Tcp { flags: u8, offset: u8, length: usize, checksum: bool },
    /// `declared` is added to the true length in the UDP length field.
    Udp { length: usize, declared: i32, checksum: bool },
    Icmp { length: usize },
    Other(u8),
}
#[derive(Clone, Debug)]
struct Packet {
    peer: usize,
    source: SocketAddrV4,
    destination: SocketAddrV4,
    header: Header,
    header_checksum: bool,
    transport: Transport,
}
fn sum(bytes: &[u8], mut total: u32) -> u16 {
    for pair in bytes.chunks(2) {
        total += u32::from(pair[0]) << 8 | u32::from(*pair.get(1).unwrap_or(&0));
    }
    while total > 0xffff {
        total = (total & 0xffff) + (total >> 16);
    }
    !(total as u16)
}
fn spoil(checksum: u16) -> u16 {
    match checksum ^ 0x5555 {
        0 => 0x1234,
        other => other,
    }
}
impl Packet {
    fn protocol(&self) -> u8 {
        match self.transport {
            Transport::Tcp { .. } => 6,
            Transport::Udp { .. } => 17,
            Transport::Icmp { .. } => 1,
            Transport::Other(protocol) => protocol,
        }
    }
    fn bytes(&self) -> Vec<u8> {
        let (source, destination) = (self.source.ip().octets(), self.destination.ip().octets());
        let pseudo = |length: usize| {
            let mut total = 0u32;
            for pair in source.chunks(2).chain(destination.chunks(2)) {
                total += u32::from(pair[0]) << 8 | u32::from(pair[1]);
            }
            total + u32::from(self.protocol()) + length as u32
        };
        let mut body = match &self.transport {
            Transport::Tcp { flags, offset, length, checksum } => {
                let mut tcp = vec![0u8; *length];
                let mut header = [0u8; 20];
                header[..2].copy_from_slice(&self.source.port().to_be_bytes());
                header[2..4].copy_from_slice(&self.destination.port().to_be_bytes());
                header[4..8].copy_from_slice(&7u32.to_be_bytes());
                header[12] = offset << 4;
                header[13] = *flags;
                header[14] = 0x10;
                let n = tcp.len().min(20);
                tcp[..n].copy_from_slice(&header[..n]);
                if tcp.len() >= 20 {
                    let good = sum(&tcp, pseudo(tcp.len()));
                    let value = if *checksum { good } else { spoil(good) };
                    tcp[16..18].copy_from_slice(&value.to_be_bytes());
                }
                tcp
            }
            Transport::Udp { length, declared, checksum } => {
                let mut udp = vec![0u8; *length];
                let mut header = [0u8; 8];
                header[..2].copy_from_slice(&self.source.port().to_be_bytes());
                header[2..4].copy_from_slice(&self.destination.port().to_be_bytes());
                header[4..6].copy_from_slice(&((*length as i32 + declared) as u16).to_be_bytes());
                let n = udp.len().min(8);
                udp[..n].copy_from_slice(&header[..n]);
                if udp.len() >= 8 {
                    let good = match sum(&udp, pseudo(udp.len())) {
                        0 => 0xffff,
                        good => good,
                    };
                    let value = if *checksum { good } else { spoil(good) };
                    udp[6..8].copy_from_slice(&value.to_be_bytes());
                }
                udp
            }
            Transport::Icmp { length } => {
                let mut icmp = vec![0u8; *length];
                icmp[0] = 8;
                icmp
            }
            Transport::Other(_) => vec![0u8; 20],
        };
        let options = self.header == Header::Options;
        let mut bytes = vec![0u8; if options { 24 } else { 20 }];
        bytes[0] = if options { 0x46 } else { 0x45 };
        let total = bytes.len() + body.len() + usize::from(self.header == Header::TotalTooLong);
        bytes[2..4].copy_from_slice(&(total as u16).to_be_bytes());
        match self.header {
            Header::DontFragment => bytes[6] = 0x40,
            Header::MoreFragments => bytes[6] = 0x20,
            Header::Offset => bytes[7] = 1,
            Header::Reserved => bytes[6] = 0x80,
            _ => (),
        }
        bytes[8] = 64;
        bytes[9] = self.protocol();
        bytes[12..16].copy_from_slice(&source);
        bytes[16..20].copy_from_slice(&destination);
        let good = sum(&bytes, 0);
        let value = if self.header_checksum { good } else { spoil(good) };
        bytes[10..12].copy_from_slice(&value.to_be_bytes());
        bytes.append(&mut body);
        if self.header == Header::TrailingByte {
            bytes.push(0);
        }
        bytes
    }
    /// "IPv4 options, fragmentation, reserved fragment flags, truncated
    /// transport headers, and mismatched UDP lengths are rejected."
    fn well_formed(&self) -> bool {
        matches!(self.header, Header::Plain | Header::DontFragment)
            && match self.transport {
                Transport::Tcp { offset, length, .. } => {
                    length >= 20 && offset >= 5 && usize::from(offset) * 4 <= length
                }
                Transport::Udp { length, declared, .. } => length >= 8 && declared == 0,
                Transport::Icmp { length } => length >= 8,
                Transport::Other(_) => false,
            }
    }
}
const FIN: u8 = 1;
const SYN: u8 = 2;
const RST: u8 = 4;
const ACK: u8 = 16;
/// A packet, and for some a managed flow to aim at: those take the flow's
/// peer, addresses and ports, so that only their other properties decide.
fn packet() -> impl Strategy<Value = (Packet, Option<prop::sample::Index>)> {
    // Mostly a peer with an address it owns.
    let sender = prop_oneof![
        4 => Just((0usize, 2u8)), 3 => Just((0, 3)), 4 => Just((1, 4)),
        1 => Just((0, 4)), 1 => Just((1, 2)), 1 => Just((0, 9)), 1 => Just((1, 1)), 1 => Just((2, 2)),
    ];
    let local = prop_oneof![18 => Just(1u8), 1 => Just(7), 1 => Just(2)];
    let header = prop_oneof![
        24 => Just(Header::Plain),
        4 => Just(Header::DontFragment),
        1 => Just(Header::MoreFragments),
        1 => Just(Header::Offset),
        1 => Just(Header::Reserved),
        1 => Just(Header::Options),
        1 => Just(Header::TrailingByte),
        1 => Just(Header::TotalTooLong),
    ];
    let flags = prop_oneof![
        Just(SYN), Just(ACK), Just(SYN | ACK), Just(RST), Just(RST | ACK), Just(SYN | RST),
        Just(SYN | RST | ACK), Just(FIN), Just(FIN | ACK), Just(0u8), Just(ACK | 8),
    ];
    let transport = prop_oneof![
        5 => (flags, prop_oneof![8 => Just((5u8, 20usize)), 2 => Just((5, 27)), 1 => Just((6, 24)),
                1 => Just((6, 20)), 1 => Just((4, 20)), 1 => Just((15, 40)), 1 => Just((5, 19)), 1 => Just((5, 4)), 1 => Just((0, 20))],
              prop::bool::weighted(0.85))
            .prop_map(|(flags, (offset, length), checksum)| Transport::Tcp { flags, offset, length, checksum }),
        4 => (prop_oneof![8 => Just((8usize, 0i32)), 3 => Just((13, 0)), 1 => Just((12, 1)), 1 => Just((12, -1)), 1 => Just((4, 0)), 1 => Just((12, -12))],
              prop::bool::weighted(0.85))
            .prop_map(|((length, declared), checksum)| Transport::Udp { length, declared, checksum }),
        2 => prop_oneof![4 => Just(8usize), 1 => Just(16), 1 => Just(4), 1 => Just(7)].prop_map(|length| Transport::Icmp { length }),
        1 => prop_oneof![Just(47u8), Just(58), Just(0), Just(2)].prop_map(Transport::Other),
    ];
    (
        sender,
        port(),
        local,
        port(),
        header,
        prop::bool::weighted(0.93),
        transport,
        prop::option::weighted(0.45, any::<prop::sample::Index>()),
    )
        .prop_map(
            |((peer, host), source_port, local, local_port, header, header_checksum, transport, aim)| {
                let packet = Packet {
                    peer,
                    source: SocketAddrV4::new(ip(host), source_port),
                    destination: SocketAddrV4::new(ip(local), local_port),
                    header,
                    header_checksum,
                    transport,
                };
                (packet, aim)
            },
        )
}

// -------------------------------------------------------------------- flows

/// A managed outbound flow whose replies are admitted without a rule.
#[derive(Clone, Debug)]
struct Flow {
    peer: usize,
    local_port: u16,
    remote: SocketAddrV4,
    /// TCP: the connection still holds its lease. UDP: unused.
    alive: bool,
    /// UDP: milliseconds after START at which the datagram was queued.
    queued: u64,
}
fn flow() -> impl Strategy<Value = Flow> {
    let reply_port = || prop_oneof![Just(80u16), Just(1234), Just(40000), Just(40001)];
    (
        prop_oneof![Just((0usize, 2u8)), Just((0, 3)), Just((1, 4))],
        reply_port(),
        reply_port(),
        prop::bool::weighted(0.8),
        prop_oneof![Just(0u64), Just(1), Just(50)],
    )
        .prop_map(|((peer, host), local_port, remote_port, alive, queued)| Flow {
            peer,
            local_port,
            remote: SocketAddrV4::new(ip(host), remote_port),
            alive,
            queued,
        })
}

#[derive(Clone, Debug)]
struct Scenario {
    jailed: [bool; 2],
    updates: Vec<Update>,
    tcp: Vec<Flow>,
    udp: Vec<Flow>,
    /// Milliseconds after START at which the packets arrive.
    arrival: u64,
    packets: Vec<Packet>,
}
fn scenario() -> impl Strategy<Value = Scenario> {
    (
        [prop::bool::weighted(0.1), prop::bool::weighted(0.3)],
        update(true),
        prop::collection::vec(update(false), 0..3),
        prop::collection::vec(flow(), 0..5),
        prop::collection::vec(flow(), 0..3),
        prop_oneof![4 => Just(50u64), 1 => Just(29_999), 1 => Just(30_000), 1 => Just(30_001), 1 => Just(30_049), 1 => Just(30_050), 1 => Just(90_000)],
        prop::collection::vec(packet(), 1..16),
    )
        .prop_map(|(jailed, first, rest, tcp, mut udp, arrival, packets)| {
            udp.sort_by_key(|flow| flow.queued);
            let packets = packets
                .into_iter()
                .map(|(mut packet, aim)| {
                    let flows = match packet.transport {
                        Transport::Tcp { .. } => &tcp,
                        Transport::Udp { .. } => &udp,
                        _ => return packet,
                    };
                    if let (Some(aim), false) = (aim, flows.is_empty()) {
                        let flow = aim.get(flows);
                        packet.peer = flow.peer;
                        packet.source = flow.remote;
                        packet.destination = SocketAddrV4::new(LOCAL, flow.local_port);
                    }
                    packet
                })
                .collect();
            Scenario {
                jailed,
                updates: std::iter::once(first).chain(rest).collect(),
                tcp,
                udp,
                arrival,
                packets,
            }
        })
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Verdict {
    Rule,
    TcpReply,
    UdpReply,
    Refused,
}
impl Scenario {
    fn expected(&self, groups: &BTreeMap<String, Vec<Rule>>, packet: &Packet) -> Verdict {
        let (source, destination) = (*packet.source.ip(), *packet.destination.ip());
        if packet.peer > 1
            || !packet.well_formed()
            || !owned(packet.peer).contains(&source.octets()[3])
            || destination != LOCAL
        {
            return Verdict::Refused;
        }
        let port = match packet.transport {
            Transport::Icmp { .. } => 0,
            _ => packet.destination.port(),
        };
        if !self.jailed[packet.peer]
            && groups
                .values()
                .flatten()
                .any(|rule| rule.allows(source, destination, packet.protocol(), port))
        {
            return Verdict::Rule;
        }
        let same = |flow: &&Flow| {
            flow.peer == packet.peer
                && flow.remote == packet.source
                && flow.local_port == packet.destination.port()
        };
        match packet.transport {
            // "Reply admission requires valid IPv4 and TCP checksums and an
            // ACK or RST; bare SYN and SYN+RST are rejected."
            Transport::Tcp { flags, checksum, .. }
                if packet.header_checksum
                    && checksum
                    && flags & (ACK | RST) != 0
                    && !(flags & SYN != 0 && (flags & ACK == 0 || flags & RST != 0))
                    && self.tcp.iter().filter(|flow| flow.alive).any(|flow| same(&flow)) =>
            {
                Verdict::TcpReply
            }
            // "...checksummed replies from the exact peer and address/port
            // tuple for 30 seconds after an outbound datagram enters the
            // ciphertext queue."
            Transport::Udp { checksum, .. }
                if packet.header_checksum
                    && checksum
                    && self
                        .udp
                        .iter()
                        .filter(|flow| self.arrival < flow.queued + 30_000)
                        .any(|flow| same(&flow)) =>
            {
                Verdict::UdpReply
            }
            _ => Verdict::Refused,
        }
    }

    /// Builds the plane and returns it with what keeps the TCP leases alive.
    fn plane(&self) -> (DataPlane, BTreeMap<String, Vec<Rule>>, Vec<Arc<()>>) {
        let mut plane = DataPlane::new(LOCAL, 4, 1, 1000).unwrap();
        let peers: Vec<Peer> = (0..2)
            .map(|index| Peer {
                key: key(index),
                addresses: owned(index).iter().map(|host| ip(*host)).collect(),
                expires_ms: 10_000_000,
                jailed: self.jailed[index],
            })
            .collect();
        let mut groups = BTreeMap::new();
        for (index, update) in self.updates.iter().enumerate() {
            let upserts: &[Peer] = if index == 0 { &peers } else { &[] };
            plane
                .apply_update(update.full, upserts, &[], &update.json(), 1000 + index as u64)
                .unwrap();
            update.apply(&mut groups);
        }
        let mut leases = Vec::new();
        let mut granted: Vec<&Flow> = Vec::new();
        for flow in &self.tcp {
            let permit = plane.begin_handshake(key(flow.peer), START).unwrap();
            let lease = Arc::new(());
            let local = SocketAddrV4::new(LOCAL, flow.local_port);
            plane
                .allow_tcp_reply(&permit, local, flow.remote, &lease, START)
                .unwrap();
            granted.push(flow);
            if flow.alive {
                leases.push(lease);
            }
        }
        let mut bindings = crate::udp::Bindings::default();
        for (index, flow) in self.udp.iter().enumerate() {
            let socket = bindings.bind(1000 + index as u16).unwrap();
            let local = SocketAddrV4::new(LOCAL, flow.local_port);
            let reply = plane
                .prepare_udp_reply(key(flow.peer), socket, local, flow.remote, START + flow.queued)
                .unwrap();
            plane.commit_udp_reply(reply).unwrap();
        }
        (plane, groups, leases)
    }
}

fn check(scenario: &Scenario) -> Result<Vec<Verdict>, TestCaseError> {
    let (mut plane, groups, _leases) = scenario.plane();
    let now = START + scenario.arrival;
    let mut verdicts = Vec::new();
    for packet in &scenario.packets {
        let bytes = packet.bytes();
        let expected = scenario.expected(&groups, packet);
        let result = plane.authenticated_packet(key(packet.peer), bytes.clone(), now);
        prop_assert_eq!(
            result.is_ok(),
            expected != Verdict::Refused,
            "admission of {:?} ({:02x?}): expected {:?}, got {:?}",
            packet,
            bytes,
            expected,
            result.as_ref().map(|_| ()).map_err(|e| e.to_string())
        );
        if let Ok(pending) = result {
            // The same decision is taken again when the packet leaves the queue.
            let released = plane.release(pending.unwrap(), now);
            prop_assert_eq!(released.ok(), Some(bytes));
        }
        verdicts.push(expected);
    }
    Ok(verdicts)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]
    #[test]
    fn admission_matches_the_documented_rules(scenario in scenario()) {
        check(&scenario)?;
    }
}

/// What the generator reaches. Run with `--ignored --nocapture`.
#[test]
#[ignore = "prints generator coverage"]
fn admission_generator_coverage() {
    use proptest::{strategy::ValueTree, test_runner::TestRunner};
    let mut runner = TestRunner::deterministic();
    let mut counts = BTreeMap::new();
    for _ in 0..3000 {
        let scenario = scenario().new_tree(&mut runner).unwrap().current();
        for verdict in check(&scenario).unwrap() {
            *counts.entry(format!("{verdict:?}")).or_insert(0usize) += 1;
        }
    }
    println!("{counts:?}");
}
