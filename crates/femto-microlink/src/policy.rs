use std::{
    collections::BTreeMap,
    fmt,
    net::{Ipv4Addr, Ipv6Addr},
};

use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};

const MAX_MAP_BYTES: usize = 64 * 1024;
const MAX_GROUPS: usize = 8;
const MAX_ROWS: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("invalid policy: {0}")]
    Json(#[from] serde_json::Error),
    #[error("policy capacity exceeded")]
    Capacity,
    #[error("unsupported or invalid address expression")]
    Address,
    #[error("invalid protocol or port range")]
    Range,
}

#[derive(Deserialize)]
struct MapUpdate {
    #[serde(rename = "PacketFilter")]
    packet_filter: Option<Vec<RuleInput>>,
    #[serde(rename = "PacketFilters", default)]
    packet_filters: UniqueGroups,
}

#[derive(Default)]
struct UniqueGroups(BTreeMap<String, Option<Vec<RuleInput>>>);

impl<'de> Deserialize<'de> for UniqueGroups {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct GroupsVisitor;
        impl<'de> Visitor<'de> for GroupsVisitor {
            type Value = UniqueGroups;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("unique named policy groups")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueGroups::default())
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut groups = BTreeMap::new();
                while let Some((name, rules)) =
                    map.next_entry::<String, Option<Vec<RuleInput>>>()?
                {
                    if groups.len() > MAX_GROUPS {
                        return Err(serde::de::Error::custom("too many groups"));
                    }
                    if name.len() > 63 {
                        return Err(serde::de::Error::custom("group name too long"));
                    }
                    if groups.insert(name, rules).is_some() {
                        return Err(serde::de::Error::custom("duplicate group"));
                    }
                }
                Ok(UniqueGroups(groups))
            }
        }
        deserializer.deserialize_any(GroupsVisitor)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleInput {
    #[serde(rename = "SrcIPs", default)]
    sources: Vec<String>,
    #[serde(rename = "SrcBits", default)]
    source_bits: Option<Vec<u8>>,
    #[serde(rename = "DstPorts", default)]
    destinations: Vec<DestinationInput>,
    #[serde(rename = "IPProto", default)]
    protocols: Option<Vec<u8>>,
    // Application capability grants do not grant access to IP traffic.
    #[serde(rename = "CapGrant", default)]
    _capabilities: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DestinationInput {
    #[serde(rename = "IP")]
    ip: String,
    #[serde(rename = "Bits", default)]
    bits: Option<u8>,
    #[serde(rename = "Ports")]
    ports: Ports,
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Ports {
    #[serde(rename = "First")]
    first: u16,
    #[serde(rename = "Last")]
    last: u16,
}

#[derive(Clone, PartialEq, Eq)]
enum Address {
    V4 { first: u32, last: u32 },
    V6,
}

impl Address {
    fn parse(text: &str, legacy_bits: Option<u8>) -> Result<Self, PolicyError> {
        if text == "*" && legacy_bits.is_none() {
            return Ok(Self::V4 {
                first: 0,
                last: u32::MAX,
            });
        }
        if let Some((a, b)) = text.split_once('-') {
            if legacy_bits.is_some() {
                return Err(PolicyError::Address);
            }
            if let (Ok(a), Ok(b)) = (a.parse::<Ipv4Addr>(), b.parse::<Ipv4Addr>()) {
                let (first, last) = (u32::from(a), u32::from(b));
                return if first <= last {
                    Ok(Self::V4 { first, last })
                } else {
                    Err(PolicyError::Address)
                };
            }
            if let (Ok(a), Ok(b)) = (a.parse::<Ipv6Addr>(), b.parse::<Ipv6Addr>()) {
                return if a <= b {
                    Ok(Self::V6)
                } else {
                    Err(PolicyError::Address)
                };
            }
            return Err(PolicyError::Address);
        }
        let (ip, bits) = match text.split_once('/') {
            Some((ip, bits)) if legacy_bits.is_none() => (
                ip,
                Some(bits.parse::<u8>().map_err(|_| PolicyError::Address)?),
            ),
            Some(_) => return Err(PolicyError::Address),
            None => (text, legacy_bits),
        };
        if let Ok(address) = ip.parse::<Ipv4Addr>() {
            let bits = bits.unwrap_or(32);
            if bits > 32 {
                return Err(PolicyError::Address);
            }
            let mask = if bits == 0 {
                0
            } else {
                u32::MAX << (32 - bits)
            };
            let first = u32::from(address) & mask;
            return Ok(Self::V4 {
                first,
                last: first | !mask,
            });
        }
        if ip.parse::<Ipv6Addr>().is_ok() && bits.unwrap_or(128) <= 128 {
            return Ok(Self::V6);
        }
        Err(PolicyError::Address)
    }
    fn matches(&self, ip: Ipv4Addr) -> bool {
        matches!(self, Self::V4 { first, last } if *first <= u32::from(ip) && u32::from(ip) <= *last)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Rule {
    source: Address,
    destination: Address,
    ports: Ports,
    protocols: Vec<u8>,
}

#[derive(Default, Clone)]
pub struct ReceivePolicy {
    groups: BTreeMap<String, Vec<Rule>>,
    generation: u64,
}

impl ReceivePolicy {
    pub(crate) fn same_rules(&self, other: &Self) -> bool {
        self.groups == other.groups
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn reset(&mut self) {
        self.groups.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    /// Accepts an authenticated coordinator map. Malformed, oversized, or
    /// unsupported policy clears ingress authorization until a valid resync.
    pub fn apply_map(&mut self, bytes: &[u8]) -> Result<(), PolicyError> {
        let result = self.prepare_update(bytes);
        match result {
            Ok(Some(groups)) => {
                self.groups = groups;
                self.generation = self.generation.wrapping_add(1);
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(error) => {
                self.reset();
                Err(error)
            }
        }
    }

    fn prepare_update(
        &self,
        bytes: &[u8],
    ) -> Result<Option<BTreeMap<String, Vec<Rule>>>, PolicyError> {
        if bytes.len() > MAX_MAP_BYTES {
            return Err(PolicyError::Capacity);
        }
        let update: MapUpdate = serde_json::from_slice(bytes)?;
        if update.packet_filter.is_none() && update.packet_filters.0.is_empty() {
            return Ok(None);
        }
        let mut groups = self.groups.clone();
        if let Some(rules) = update.packet_filter {
            groups.insert("base".into(), compile(rules)?);
        }
        if matches!(update.packet_filters.0.get("*"), Some(None)) {
            groups.clear();
        }
        for (name, rules) in update.packet_filters.0 {
            if name == "*" {
                if rules.is_some() {
                    return Err(PolicyError::Address);
                }
                continue;
            }
            match rules {
                Some(rules) => {
                    groups.insert(name, compile(rules)?);
                }
                None => {
                    groups.remove(&name);
                }
            }
        }
        if groups.len() > MAX_GROUPS || groups.values().map(Vec::len).sum::<usize>() > MAX_ROWS {
            return Err(PolicyError::Capacity);
        }
        Ok(Some(groups))
    }

    pub fn allows(&self, source: Ipv4Addr, destination: Ipv4Addr, protocol: u8, port: u16) -> bool {
        self.groups.values().flatten().any(|rule| {
            rule.source.matches(source)
                && rule.destination.matches(destination)
                && rule.protocols.contains(&protocol)
                && rule.ports.first <= port
                && port <= rule.ports.last
        })
    }
}

fn compile(inputs: Vec<RuleInput>) -> Result<Vec<Rule>, PolicyError> {
    let mut rules = Vec::new();
    for input in inputs {
        if let Some(bits) = &input.source_bits
            && bits.len() != input.sources.len()
        {
            return Err(PolicyError::Address);
        }
        let protocols = input
            .protocols
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| vec![1, 6, 17]);
        if protocols.len() > 256 {
            return Err(PolicyError::Capacity);
        }
        if input
            .sources
            .len()
            .checked_mul(input.destinations.len())
            .is_none_or(|count| count > MAX_ROWS.saturating_sub(rules.len()))
        {
            return Err(PolicyError::Capacity);
        }
        for (i, source) in input.sources.iter().enumerate() {
            let source = Address::parse(source, input.source_bits.as_ref().map(|bits| bits[i]))?;
            for dest in &input.destinations {
                if dest.ports.first > dest.ports.last {
                    return Err(PolicyError::Range);
                }
                rules.push(Rule {
                    source: source.clone(),
                    destination: Address::parse(&dest.ip, dest.bits)?,
                    ports: dest.ports.clone(),
                    protocols: protocols.clone(),
                });
            }
        }
    }
    Ok(rules)
}

#[cfg(test)]
mod tests {
    use super::*;
    const ALLOW: &[u8] = br#"{"PacketFilter":[{"SrcIPs":["100.64.0.1/32"],"DstPorts":[{"IP":"100.64.0.2","Ports":{"First":80,"Last":80}}],"IPProto":[6]}]}"#;
    fn allows(policy: &ReceivePolicy) -> bool {
        policy.allows(
            "100.64.0.1".parse().unwrap(),
            "100.64.0.2".parse().unwrap(),
            6,
            80,
        )
    }

    #[test]
    fn default_deny_replacement_omission_and_revocation() {
        let mut policy = ReceivePolicy::default();
        assert!(!allows(&policy));
        policy.apply_map(ALLOW).unwrap();
        assert!(allows(&policy));
        policy.apply_map(br#"{"KeepAlive":true}"#).unwrap();
        assert!(allows(&policy));
        policy.apply_map(br#"{"PacketFilter":null}"#).unwrap();
        assert!(allows(&policy));
        policy.apply_map(br#"{"PacketFilter":[]}"#).unwrap();
        assert!(!allows(&policy));
    }

    #[test]
    fn malformed_update_cannot_retain_stale_authorization() {
        for bad in [
            br#"{"PacketFilter":[{"SrcIPs":["*"],"CapMatch":["unknown"]}]}"#.as_slice(),
            br#"{"PacketFilter":[],"PacketFilter":null}"#,
            br#"{"PacketFilters":{"x":[],"x":null}}"#,
            b"{broken",
        ] {
            let mut policy = ReceivePolicy::default();
            policy.apply_map(ALLOW).unwrap();
            assert!(policy.apply_map(bad).is_err());
            assert!(!allows(&policy));
        }
    }

    #[test]
    fn named_group_reset_and_delete() {
        let mut policy = ReceivePolicy::default();
        policy.apply_map(ALLOW).unwrap();
        policy.apply_map(br#"{"PacketFilters":{"*":null,"new":[{"SrcIPs":["*"],"DstPorts":[{"IP":"*","Ports":{"First":0,"Last":65535}}]}]}}"#).unwrap();
        assert!(allows(&policy));
        policy
            .apply_map(br#"{"PacketFilters":{"new":null}}"#)
            .unwrap();
        assert!(!allows(&policy));
    }

    #[test]
    fn oversized_map_denies_all() {
        let mut policy = ReceivePolicy::default();
        policy.apply_map(ALLOW).unwrap();
        assert!(policy.apply_map(&vec![b' '; MAX_MAP_BYTES + 1]).is_err());
        assert!(!allows(&policy));
    }

    #[test]
    fn cidr_ranges_and_ipv6_validation() {
        assert!(
            Address::parse("100.64.0.0/10", None)
                .unwrap()
                .matches("100.127.255.255".parse().unwrap())
        );
        assert!(
            !Address::parse("100.64.0.0/10", None)
                .unwrap()
                .matches("100.128.0.0".parse().unwrap())
        );
        assert!(
            Address::parse("100.64.0.2-100.64.0.8", None)
                .unwrap()
                .matches("100.64.0.8".parse().unwrap())
        );
        assert!(Address::parse("::/129", None).is_err());
        assert!(
            !Address::parse("::/0", None)
                .unwrap()
                .matches(Ipv4Addr::LOCALHOST)
        );
    }
}
