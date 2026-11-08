//! Bounded application UDP bindings and smoltcp IPv4/UDP packet encoding.
use crate::protocol::key::{Node, PublicKey};
use smoltcp::{
    phy::ChecksumCapabilities,
    wire::{IpProtocol, Ipv4Packet, Ipv4Repr, UdpPacket, UdpRepr},
};
use std::{collections::BTreeMap, net::SocketAddrV4};

pub const MAX_PAYLOAD: usize = crate::transport::TUNNEL_MTU - 28;
const SOCKETS: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("UDP socket capacity reached or port already bound")]
    Capacity,
    #[error("UDP socket is closed or unknown")]
    Closed,
    #[error("invalid or oversized IPv4 UDP datagram")]
    Packet,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Socket(u64);
pub struct Datagram {
    pub peer: PublicKey<Node>,
    pub source: SocketAddrV4,
    pub payload: Vec<u8>,
}
#[derive(Default)]
pub(crate) struct Bindings {
    ports: BTreeMap<u64, u16>,
    next: u64,
}
impl Bindings {
    pub fn bind(&mut self, mut port: u16) -> Result<Socket, Error> {
        if self.ports.len() >= SOCKETS {
            return Err(Error::Capacity);
        }
        if port == 0 {
            port = (49152..=65535)
                .find(|p| !self.ports.values().any(|bound| bound == p))
                .ok_or(Error::Capacity)?;
        }
        if self.ports.values().any(|p| *p == port) {
            return Err(Error::Capacity);
        }
        self.next = self.next.checked_add(1).ok_or(Error::Capacity)?;
        self.ports.insert(self.next, port);
        Ok(Socket(self.next))
    }
    pub fn port(&self, socket: Socket) -> Result<u16, Error> {
        self.ports.get(&socket.0).copied().ok_or(Error::Closed)
    }
    pub fn close(&mut self, socket: Socket) -> Result<u16, Error> {
        self.ports.remove(&socket.0).ok_or(Error::Closed)
    }
    pub fn clear(&mut self) {
        self.ports.clear();
    }
}
pub(crate) fn encode(
    source: SocketAddrV4,
    destination: SocketAddrV4,
    payload: &[u8],
) -> Result<Vec<u8>, Error> {
    if source.port() == 0 || destination.port() == 0 || payload.len() > MAX_PAYLOAD {
        return Err(Error::Packet);
    }
    let caps = ChecksumCapabilities::default();
    let ip = Ipv4Repr {
        src_addr: *source.ip(),
        dst_addr: *destination.ip(),
        next_header: IpProtocol::Udp,
        payload_len: payload.len() + 8,
        hop_limit: 64,
    };
    let mut bytes = vec![0; 28 + payload.len()];
    ip.emit(&mut Ipv4Packet::new_unchecked(&mut bytes[..]), &caps);
    UdpRepr {
        src_port: source.port(),
        dst_port: destination.port(),
    }
    .emit(
        &mut UdpPacket::new_unchecked(&mut bytes[20..]),
        &(*source.ip()).into(),
        &(*destination.ip()).into(),
        payload.len(),
        |target| target.copy_from_slice(payload),
        &caps,
    );
    Ok(bytes)
}
pub(crate) struct Parsed<'a> {
    pub source: SocketAddrV4,
    pub destination: SocketAddrV4,
    pub payload: &'a [u8],
}
pub(crate) fn decode(bytes: &[u8]) -> Result<Parsed<'_>, Error> {
    if bytes.len() > crate::transport::TUNNEL_MTU {
        return Err(Error::Packet);
    }
    let caps = ChecksumCapabilities::default();
    let ip_packet = Ipv4Packet::new_checked(bytes).map_err(|_| Error::Packet)?;
    let ip = Ipv4Repr::parse(&ip_packet, &caps).map_err(|_| Error::Packet)?;
    if ip.next_header != IpProtocol::Udp
        || ip_packet.header_len() != 20
        || usize::from(ip_packet.total_len()) != bytes.len()
    {
        return Err(Error::Packet);
    }
    let udp = UdpPacket::new_checked(&bytes[20..]).map_err(|_| Error::Packet)?;
    let ports = UdpRepr::parse(&udp, &ip.src_addr.into(), &ip.dst_addr.into(), &caps)
        .map_err(|_| Error::Packet)?;
    if usize::from(udp.len()) != ip.payload_len {
        return Err(Error::Packet);
    }
    Ok(Parsed {
        source: SocketAddrV4::new(ip.src_addr, ports.src_port),
        destination: SocketAddrV4::new(ip.dst_addr, ports.dst_port),
        payload: &bytes[28..],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codec_checks_lengths_checksums_and_mtu() {
        let source = "100.64.0.1:1234".parse().unwrap();
        let target = "100.64.0.2:9000".parse().unwrap();
        for payload in [vec![], vec![42; MAX_PAYLOAD]] {
            let packet = encode(source, target, &payload).unwrap();
            let parsed = decode(&packet).unwrap();
            assert_eq!(parsed.source, source);
            assert_eq!(parsed.destination, target);
            assert_eq!(parsed.payload, payload);
            let mut damaged = packet.clone();
            damaged[26] ^= 1;
            assert!(decode(&damaged).is_err());
            damaged[26..28].fill(0);
            assert!(decode(&damaged).is_ok()); // IPv4 permits omitted UDP checksums.
            damaged[10] ^= 1;
            assert!(decode(&damaged).is_err());
            for length in 0..28 {
                assert!(decode(&packet[..length]).is_err());
            }
            let mut trailing = packet;
            trailing.push(0);
            assert!(decode(&trailing).is_err());
        }
        assert!(encode(source, target, &[0; MAX_PAYLOAD + 1]).is_err());
    }
    #[test]
    fn bindings_are_bounded_and_closed_handles_do_not_recur() {
        let mut bindings = Bindings::default();
        let first = bindings.bind(0).unwrap();
        let port = bindings.port(first).unwrap();
        assert!(port >= 49152);
        assert!(bindings.bind(port).is_err());
        for _ in 1..SOCKETS {
            bindings.bind(0).unwrap();
        }
        assert!(bindings.bind(0).is_err());
        bindings.close(first).unwrap();
        let replacement = bindings.bind(port).unwrap();
        assert_ne!(replacement, first);
        assert!(bindings.port(first).is_err());
        bindings.clear();
        assert!(bindings.port(replacement).is_err());
        assert_ne!(bindings.bind(port).unwrap(), replacement);
    }
}
