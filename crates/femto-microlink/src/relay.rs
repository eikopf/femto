//! Validated coordinator relay descriptions; no TLS connection is made here.
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

pub const MAX_REGION_ID: u64 = (1 << 53) - 1;
const MAX_REGIONS: usize = 64;
const MAX_NODES: usize = 256;

#[derive(Debug, thiserror::Error)]
#[error("invalid or oversized relay directory")]
pub struct RelayError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address<T> {
    Dns,
    Disabled,
    Fixed(T),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Certificate {
    /// TLS must verify the certificate chain and this name.
    Name(String),
    /// TLS must verify the SHA-256 of the complete DER certificate.
    Sha256([u8; 32]),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayNode {
    pub name: String,
    pub host_name: String,
    pub certificate: Certificate,
    pub ipv4: Address<Ipv4Addr>,
    pub ipv6: Address<Ipv6Addr>,
    pub tls_port: u16,
    pub stun_port: Option<u16>,
    pub stun_only: bool,
}
impl RelayNode {
    pub(crate) fn can_relay(&self) -> bool {
        !self.stun_only
            && (!matches!(self.ipv4, Address::Disabled) || !matches!(self.ipv6, Address::Disabled))
    }
    pub fn allows_address(&self, address: std::net::SocketAddr) -> bool {
        let allowed = match address.ip() {
            std::net::IpAddr::V4(ip) => match self.ipv4 {
                Address::Dns => true,
                Address::Fixed(fixed) => ip == fixed,
                Address::Disabled => false,
            },
            std::net::IpAddr::V6(ip) => match self.ipv6 {
                Address::Dns => true,
                Address::Fixed(fixed) => ip == fixed,
                Address::Disabled => false,
            },
        };
        !self.stun_only
            && allowed
            && address.port() != 0
            && address.port() == self.tls_port
            && !address.ip().is_unspecified()
            && !address.ip().is_multicast()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub no_measure_no_home: bool,
    /// Coordinator preference order, preserved for relay failover.
    pub nodes: Vec<RelayNode>,
}
#[derive(Clone, Default, PartialEq, Eq)]
pub struct RelayDirectory {
    regions: BTreeMap<u64, Region>,
}
impl RelayDirectory {
    pub fn region(&self, id: u64) -> Option<&Region> {
        self.regions.get(&id)
    }
    pub fn region_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.regions.keys().copied()
    }
    pub(crate) fn updated(&self, update: Option<RelayUpdate>) -> Result<Self, RelayError> {
        let Some(regions) = update.and_then(|update| update.regions) else {
            return Ok(self.clone());
        };
        let mut names = BTreeSet::new();
        let mut result = BTreeMap::new();
        let mut count = 0;
        for (id, region) in regions.0 {
            if id == 0 || id > MAX_REGION_ID || id != region.region_id || region.nodes.len() > 8 {
                return Err(RelayError);
            }
            let mut nodes = Vec::new();
            for node in region.nodes {
                count += 1;
                if count > MAX_NODES || node.region_id != id || !names.insert(node.name.clone()) {
                    return Err(RelayError);
                }
                nodes.push(node.validate()?);
            }
            result.insert(
                id,
                Region {
                    no_measure_no_home: region.no_measure_no_home,
                    nodes,
                },
            );
        }
        Ok(Self { regions: result })
    }
}

#[derive(Deserialize)]
pub(crate) struct RelayUpdate {
    #[serde(rename = "Regions")]
    regions: Option<Regions>,
}
struct Regions(BTreeMap<u64, WireRegion>);
impl<'de> Deserialize<'de> for Regions {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Unique;
        impl<'de> Visitor<'de> for Unique {
            type Value = Regions;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("unique numeric relay regions")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut regions = BTreeMap::new();
                while let Some((id, region)) = map.next_entry::<String, WireRegion>()? {
                    let id = id.parse::<u64>().map_err(serde::de::Error::custom)?;
                    if regions.len() >= MAX_REGIONS || regions.insert(id, region).is_some() {
                        return Err(serde::de::Error::custom(
                            "duplicate or excessive relay regions",
                        ));
                    }
                }
                Ok(Regions(regions))
            }
        }
        deserializer.deserialize_map(Unique)
    }
}
#[derive(Deserialize)]
struct WireRegion {
    #[serde(rename = "RegionID")]
    region_id: u64,
    #[serde(rename = "NoMeasureNoHome", default)]
    no_measure_no_home: bool,
    #[serde(rename = "Nodes")]
    nodes: Vec<WireNode>,
}
#[derive(Deserialize)]
struct WireNode {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "RegionID")]
    region_id: u64,
    #[serde(rename = "HostName")]
    host_name: String,
    #[serde(rename = "CertName", default)]
    cert_name: String,
    #[serde(rename = "IPv4", default)]
    ipv4: String,
    #[serde(rename = "IPv6", default)]
    ipv6: String,
    #[serde(rename = "DERPPort", default)]
    derp_port: u16,
    #[serde(rename = "STUNPort", default)]
    stun_port: i32,
    #[serde(rename = "STUNOnly", default)]
    stun_only: bool,
    #[serde(rename = "InsecureForTests", default)]
    insecure: bool,
    #[serde(rename = "STUNTestIP", default)]
    test_ip: String,
}
impl WireNode {
    fn validate(self) -> Result<RelayNode, RelayError> {
        if self.name.is_empty()
            || self.name.len() > 64
            || !self
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || self.insecure
            || !self.test_ip.is_empty()
        {
            return Err(RelayError);
        }
        let host_name = host(&self.host_name)?;
        let certificate = if self.cert_name.is_empty() {
            Certificate::Name(host_name.clone())
        } else if let Some(hex) = self.cert_name.strip_prefix("sha256-raw:") {
            if hex.len() != 64 || !hex.is_ascii() {
                return Err(RelayError);
            }
            let mut digest = [0; 32];
            for (i, byte) in digest.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| RelayError)?;
            }
            Certificate::Sha256(digest)
        } else {
            Certificate::Name(host(&self.cert_name)?)
        };
        let ipv4 = match self.ipv4.as_str() {
            "" => Address::Dns,
            "none" => Address::Disabled,
            value => match value.parse::<Ipv4Addr>() {
                Ok(ip) if usable(IpAddr::V4(ip)) => Address::Fixed(ip),
                Ok(_) => return Err(RelayError),
                Err(_) => Address::Disabled,
            },
        };
        let ipv6 = match self.ipv6.as_str() {
            "" => Address::Dns,
            "none" => Address::Disabled,
            value => match value.parse::<Ipv6Addr>() {
                Ok(ip) if usable(IpAddr::V6(ip)) => Address::Fixed(ip),
                Ok(_) => return Err(RelayError),
                Err(_) => Address::Disabled,
            },
        };
        let stun_port = match self.stun_port {
            -1 => None,
            0 => Some(3478),
            1..=65535 => Some(self.stun_port as u16),
            _ => return Err(RelayError),
        };
        Ok(RelayNode {
            name: self.name,
            host_name,
            certificate,
            ipv4,
            ipv6,
            tls_port: if self.derp_port == 0 {
                443
            } else {
                self.derp_port
            },
            stun_port,
            stun_only: self.stun_only,
        })
    }
}
fn usable(ip: IpAddr) -> bool {
    let ip = ip.to_canonical();
    !ip.is_unspecified()
        && !ip.is_loopback()
        && !ip.is_multicast()
        && !matches!(ip, IpAddr::V4(ip) if ip.is_broadcast())
}
fn host(value: &str) -> Result<String, RelayError> {
    if value.len() > 253 || value.is_empty() {
        return Err(RelayError);
    }
    if let Ok(ip) = value.parse::<IpAddr>() {
        return if usable(ip) {
            Ok(ip.to_string())
        } else {
            Err(RelayError)
        };
    }
    let value = value.strip_suffix('.').unwrap_or(value);
    if value.split('.').any(|label| {
        label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }) {
        return Err(RelayError);
    }
    Ok(value.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn node() -> Value {
        json!({"Name":"7a","RegionID":7,"HostName":"Relay.Example."})
    }
    fn update(node: Value) -> RelayUpdate {
        serde_json::from_value(json!({"Regions":{"7":{"RegionID":7,"Nodes":[node]}}})).unwrap()
    }

    #[test]
    fn directory_bounds_and_global_node_uniqueness() {
        let mut regions = serde_json::Map::new();
        for id in 1..=65 {
            regions.insert(id.to_string(), json!({"RegionID":id,"Nodes":[]}));
        }
        assert!(serde_json::from_value::<RelayUpdate>(json!({"Regions":regions})).is_err());
        let too_many = json!({"Regions":{"7":{"RegionID":7,"Nodes":vec![node();9]}}});
        let update: RelayUpdate = serde_json::from_value(too_many).unwrap();
        assert!(RelayDirectory::default().updated(Some(update)).is_err());
        let duplicated = json!({"Regions":{"7":{"RegionID":7,"Nodes":[node(),node()]}}});
        let update: RelayUpdate = serde_json::from_value(duplicated).unwrap();
        assert!(RelayDirectory::default().updated(Some(update)).is_err());
        let mut regions = serde_json::Map::new();
        for id in 1..=33 {
            let nodes: Vec<_> = (0..8).map(|n| json!({"Name":format!("n{id}-{n}"),"RegionID":id,"HostName":"relay.example"})).collect();
            regions.insert(id.to_string(), json!({"RegionID":id,"Nodes":nodes}));
        }
        let update: RelayUpdate = serde_json::from_value(json!({"Regions":regions})).unwrap();
        assert!(RelayDirectory::default().updated(Some(update)).is_err());
    }

    #[test]
    fn identity_addresses_and_ports_are_distinct() {
        let mut wire = node();
        wire["CertName"] = json!("certificate.example");
        wire["IPv4"] = json!("192.0.2.7");
        wire["IPv6"] = json!("none");
        wire["DERPPort"] = json!(8443);
        wire["STUNPort"] = json!(-1);
        let dir = RelayDirectory::default()
            .updated(Some(update(wire)))
            .unwrap();
        let node = &dir.region(7).unwrap().nodes[0];
        assert_eq!(node.host_name, "relay.example");
        assert_eq!(
            node.certificate,
            Certificate::Name("certificate.example".into())
        );
        assert_eq!(node.ipv4, Address::Fixed("192.0.2.7".parse().unwrap()));
        assert_eq!(node.ipv6, Address::Disabled);
        assert_eq!(node.tls_port, 8443);
        assert_eq!(node.stun_port, None);
    }
    #[test]
    fn omitted_regions_preserve_but_empty_regions_clear() {
        let dir = RelayDirectory::default()
            .updated(Some(update(node())))
            .unwrap();
        for wire in [
            json!({}),
            json!({"Regions":null}),
            json!({"HomeParams":{"RegionScore":{"7":0.5}}}),
        ] {
            let next = dir
                .updated(Some(serde_json::from_value(wire).unwrap()))
                .unwrap();
            assert_eq!(next.region(7), dir.region(7));
        }
        let next = dir
            .updated(Some(serde_json::from_value(json!({"Regions":{}})).unwrap()))
            .unwrap();
        assert!(next.region(7).is_none());
        let node = &dir.region(7).unwrap().nodes[0];
        assert_eq!(node.tls_port, 443);
        assert_eq!(node.stun_port, Some(3478));
        assert_eq!(node.ipv4, Address::Dns);
    }
    #[test]
    fn invalid_or_unsafe_descriptions_are_rejected() {
        for (field, value) in [
            ("InsecureForTests", json!(true)),
            ("STUNTestIP", json!("192.0.2.1")),
            ("HostName", json!("relay.example/path")),
            ("CertName", json!("sha256-raw:00")),
            ("STUNPort", json!(-2)),
            ("STUNPort", json!(65536)),
            ("RegionID", json!(8)),
            ("IPv4", json!("127.0.0.1")),
            ("IPv6", json!("::ffff:127.0.0.1")),
        ] {
            let mut wire = node();
            wire[field] = value;
            assert!(
                RelayDirectory::default()
                    .updated(Some(update(wire)))
                    .is_err()
            );
        }
        let duplicated =
            br#"{"Regions":{"7":{"RegionID":7,"Nodes":[]},"7":{"RegionID":7,"Nodes":[]}}}"#;
        assert!(serde_json::from_slice::<RelayUpdate>(duplicated).is_err());
        let mut wire = node();
        wire["CertName"] = json!(format!("sha256-raw:{}", "ab".repeat(32)));
        let dir = RelayDirectory::default()
            .updated(Some(update(wire)))
            .unwrap();
        assert_eq!(
            dir.region(7).unwrap().nodes[0].certificate,
            Certificate::Sha256([0xab; 32])
        );
    }
}
