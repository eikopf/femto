//! Nonblocking standard-library datagrams for host and ESP-IDF sockets. Authentication and path selection belong to the
//! handshake owner and network actor, not to socket source addresses.
use crate::derp::MAX_DATAGRAM;
use std::{
    io,
    net::{SocketAddr, UdpSocket},
};

pub struct Datagram {
    pub source: SocketAddr,
    pub bytes: Vec<u8>,
}
pub struct UdpTransport {
    socket: Option<UdpSocket>,
}
impl UdpTransport {
    pub fn bind(address: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(address)?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket: Some(socket),
        })
    }
    fn socket(&self) -> io::Result<&UdpSocket> {
        self.socket
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "UDP transport closed"))
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket()?.local_addr()
    }
    /// One syscall, no retry or hidden queue. WouldBlock means the caller must
    /// reschedule and revalidate any queued packet against current authorization.
    pub fn send(&self, destination: SocketAddr, bytes: &[u8]) -> io::Result<()> {
        let socket = self.socket()?;
        if bytes.is_empty()
            || bytes.len() > MAX_DATAGRAM
            || destination.port() == 0
            || destination.ip().is_unspecified()
            || destination.ip().is_multicast()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid UDP datagram or target",
            ));
        }
        if socket.send_to(bytes, destination)? != bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "incomplete UDP datagram",
            ));
        }
        Ok(())
    }
    /// Reads at most one datagram. The extra byte detects oversized input even
    /// when the operating system truncates a much larger UDP payload.
    pub fn receive(&self) -> io::Result<Option<Datagram>> {
        let mut bytes = [0; MAX_DATAGRAM + 1];
        let (length, source) = match self.socket()?.recv_from(&mut bytes) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => return Err(error),
        };
        if length > MAX_DATAGRAM {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversized UDP datagram",
            ));
        }
        Ok(Some(Datagram {
            source,
            bytes: bytes[..length].to_vec(),
        }))
    }
    /// Closing the socket does not alter authorization. The network actor must
    /// disconnect its coordinator and maintain handshake state when stopping.
    pub fn close(&mut self) {
        self.socket = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        handshake::{Event, Handshakes},
        plane::{DataPlane, Peer},
        protocol::identity::DeviceIdentity,
        timestamp::{EpochStore, TimestampClock},
    };
    use std::{
        net::Ipv4Addr,
        time::{Duration, Instant},
    };
    fn socket() -> UdpTransport {
        UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap()
    }
    fn receive(socket: &UdpTransport) -> io::Result<Datagram> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(packet) = socket.receive()? {
                return Ok(packet);
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
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
    #[test]
    fn loopback_handshake_confirmation_and_revocation() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ak = a.node_key().public();
        let bk = b.node_key().public();
        let mut pa = DataPlane::new(Ipv4Addr::new(100, 64, 0, 1), 1, 1, 0).unwrap();
        let mut pb = DataPlane::new(Ipv4Addr::new(100, 64, 0, 2), 1, 1, 0).unwrap();
        for (plane, key, address) in [
            (&mut pa, bk, Ipv4Addr::new(100, 64, 0, 2)),
            (&mut pb, ak, Ipv4Addr::new(100, 64, 0, 1)),
        ] {
            plane
                .apply_update(
                    true,
                    &[Peer {
                        key,
                        addresses: vec![address],
                        expires_ms: 10000,
                        jailed: false,
                    }],
                    &[],
                    br#"{"PacketFilter":[]}"#,
                    0,
                )
                .unwrap();
        }
        let mut ha = Handshakes::new(a.node_key().clone(), 1, 0).unwrap();
        let mut hb = Handshakes::new(b.node_key().clone(), 1, 0).unwrap();
        let mut clock = TimestampClock::open(Store, 100).unwrap();
        let sa = socket();
        let sb = socket();
        sa.send(
            sb.local_addr().unwrap(),
            &ha.begin(&mut pa, bk, &mut clock, 0).unwrap(),
        )
        .unwrap();
        let input = receive(&sb).unwrap();
        assert_eq!(input.source, sa.local_addr().unwrap());
        let Event::Response { peer, packet } = hb.handle(&mut pb, &input.bytes, 1).unwrap() else {
            panic!()
        };
        assert_eq!(peer, ak);
        sb.send(input.source, &packet).unwrap();
        let Event::Established(peer) = ha.handle(&mut pa, &receive(&sa).unwrap().bytes, 2).unwrap()
        else {
            panic!()
        };
        assert_eq!(peer, bk);
        sa.send(sb.local_addr().unwrap(), &pa.send(bk, &[], 3).unwrap())
            .unwrap();
        let Event::Transport { peer, packet } =
            hb.handle(&mut pb, &receive(&sb).unwrap().bytes, 4).unwrap()
        else {
            panic!()
        };
        assert_eq!(peer, ak);
        assert!(packet.is_none());
        sb.send(sa.local_addr().unwrap(), &pb.send(ak, &[], 5).unwrap())
            .unwrap();
        assert!(matches!(
            ha.handle(&mut pa, &receive(&sa).unwrap().bytes, 6).unwrap(),
            Event::Transport { packet: None, .. }
        ));
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
        let mut sum: u32 = ip[..20]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u32::from(u16::from_be_bytes([b[0], b[1]])))
            .sum();
        while sum > 0xffff {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        ip[10..12].copy_from_slice(&(!(sum as u16)).to_be_bytes());
        sa.send(sb.local_addr().unwrap(), &pa.send(bk, &ip, 7).unwrap())
            .unwrap();
        assert!(hb.handle(&mut pb, &receive(&sb).unwrap().bytes, 7).is_err());
        pb.apply_update(false,&[],&[],br#"{"PacketFilter":[{"SrcIPs":["100.64.0.1"],"DstPorts":[{"IP":"100.64.0.2","Ports":{"First":1234,"Last":1234}}],"IPProto":[17]}]}"#,8).unwrap();
        sa.send(sb.local_addr().unwrap(), &pa.send(bk, &ip, 9).unwrap())
            .unwrap();
        let Event::Transport {
            peer,
            packet: Some(packet),
        } = hb
            .handle(&mut pb, &receive(&sb).unwrap().bytes, 10)
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(peer, ak);
        assert_eq!(pb.release(packet, 10).unwrap(), ip);
        sa.send(sb.local_addr().unwrap(), &pa.send(bk, &ip, 11).unwrap())
            .unwrap();
        let Event::Transport {
            packet: Some(packet),
            ..
        } = hb
            .handle(&mut pb, &receive(&sb).unwrap().bytes, 12)
            .unwrap()
        else {
            panic!()
        };
        pb.apply_update(false, &[], &[], br#"{"PacketFilter":[]}"#, 13)
            .unwrap();
        assert!(pb.release(packet, 13).is_err());
        sa.send(sb.local_addr().unwrap(), &pa.send(bk, &[], 14).unwrap())
            .unwrap();
        pb.disconnect();
        assert!(
            hb.handle(&mut pb, &receive(&sb).unwrap().bytes, 15)
                .is_err()
        );
    }
    #[test]
    fn nonblocking_bounds_and_close() {
        let mut target = socket();
        assert!(target.receive().unwrap().is_none());
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        sender
            .send_to(&vec![1; 4096], target.local_addr().unwrap())
            .unwrap();
        assert_eq!(
            receive(&target).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        sender
            .send_to(b"next", target.local_addr().unwrap())
            .unwrap();
        assert_eq!(receive(&target).unwrap().bytes, b"next");
        assert!(
            target
                .send(sender.local_addr().unwrap(), &vec![0; MAX_DATAGRAM + 1])
                .is_err()
        );
        target.close();
        assert_eq!(
            target.receive().err().unwrap().kind(),
            io::ErrorKind::NotConnected
        );
        assert_eq!(
            target
                .send(sender.local_addr().unwrap(), b"x")
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotConnected
        );
    }
}
