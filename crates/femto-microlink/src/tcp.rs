//! Bounded IPv4 TCP stream engine. Callers supply authenticated tunnel packets.
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken},
    socket::tcp,
    time::{Duration, Instant},
    wire::{HardwareAddress, IpCidr, IpProtocol, Ipv4Packet},
};
use std::{collections::VecDeque, net::SocketAddrV4};

/// Opaque connection handle bound to one network owner.
#[derive(Clone, Debug)]
pub struct Socket {
    pub(crate) id: u64,
    pub(crate) identity: std::sync::Arc<()>,
}
const QUEUE: usize = 8;
const BUFFER: usize = 16 * 1024;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid TCP endpoint or packet")]
    Packet,
    #[error("TCP packet queue full")]
    Capacity,
    #[error("TCP clock moved backwards or overflowed")]
    Clock,
    #[error("TCP stream is not writable or readable")]
    State,
}
#[derive(Default)]
struct Packets {
    incoming: VecDeque<Vec<u8>>,
    outgoing: VecDeque<Vec<u8>>,
}
struct Rx(Vec<u8>);
struct Tx<'a>(&'a mut VecDeque<Vec<u8>>);
impl RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}
impl TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut bytes = vec![0; len];
        let result = f(&mut bytes);
        self.0.push_back(bytes);
        result
    }
}
impl Device for Packets {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;
    fn receive(&mut self, _: Instant) -> Option<(Rx, Tx<'_>)> {
        if self.outgoing.len() == QUEUE {
            return None;
        }
        self.incoming
            .pop_front()
            .map(|p| (Rx(p), Tx(&mut self.outgoing)))
    }
    fn transmit(&mut self, _: Instant) -> Option<Tx<'_>> {
        (self.outgoing.len() < QUEUE).then_some(Tx(&mut self.outgoing))
    }
    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = crate::transport::TUNNEL_MTU;
        caps
    }
}
/// One flow with 16 KiB receive/send buffers and eight packets per direction.
/// Packet admission does not replace coordinator policy or peer authentication.
pub struct Stream {
    interface: Interface,
    sockets: SocketSet<'static>,
    handle: SocketHandle,
    packets: Packets,
    local: SocketAddrV4,
    remote: SocketAddrV4,
    last_ms: i64,
    aborted: bool,
}
impl Stream {
    pub fn connect(local: SocketAddrV4, remote: SocketAddrV4, seed: u64) -> Result<Self, Error> {
        Self::new(local, remote, seed, false)
    }
    pub(crate) fn new(
        local: SocketAddrV4,
        remote: SocketAddrV4,
        seed: u64,
        listen: bool,
    ) -> Result<Self, Error> {
        if local.port() == 0
            || remote.port() == 0
            || local.ip().is_unspecified()
            || remote.ip().is_unspecified()
            || local.ip().is_multicast()
            || remote.ip().is_multicast()
            || local.ip().is_broadcast()
            || remote.ip().is_broadcast()
            || local.ip() == remote.ip()
        {
            return Err(Error::Packet);
        }
        let mut packets = Packets::default();
        let mut config = Config::new(HardwareAddress::Ip);
        config.random_seed = seed;
        let mut interface = Interface::new(config, &mut packets, Instant::from_millis(0));
        interface.update_ip_addrs(|a| a.push(IpCidr::new((*local.ip()).into(), 0)).unwrap());
        let mut socket = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; BUFFER]),
            tcp::SocketBuffer::new(vec![0; BUFFER]),
        );
        socket.set_timeout(Some(Duration::from_secs(30)));
        socket.set_nagle_enabled(false);
        if listen {
            socket.listen(local.port()).map_err(|_| Error::State)?;
        } else {
            socket
                .connect(interface.context(), remote, local)
                .map_err(|_| Error::State)?;
        }
        let mut sockets = SocketSet::new(Vec::new());
        let handle = sockets.add(socket);
        Ok(Self {
            interface,
            sockets,
            handle,
            packets,
            local,
            remote,
            last_ms: 0,
            aborted: false,
        })
    }
    pub fn ingest(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.aborted {
            return Err(Error::State);
        }
        if bytes.len() > crate::transport::TUNNEL_MTU {
            return Err(Error::Packet);
        }
        let ip = Ipv4Packet::new_checked(bytes).map_err(|_| Error::Packet)?;
        if ip.version() != 4
            || ip.header_len() != 20
            || usize::from(ip.total_len()) != bytes.len()
            || !ip.verify_checksum()
            || ip.more_frags()
            || ip.frag_offset() != 0
            || ip.next_header() != IpProtocol::Tcp
            || ip.src_addr() != *self.remote.ip()
            || ip.dst_addr() != *self.local.ip()
        {
            return Err(Error::Packet);
        }
        let tcp = smoltcp::wire::TcpPacket::new_checked(ip.payload()).map_err(|_| Error::Packet)?;
        if tcp.src_port() != self.remote.port()
            || tcp.dst_port() != self.local.port()
            || !tcp.verify_checksum(&(*self.remote.ip()).into(), &(*self.local.ip()).into())
        {
            return Err(Error::Packet);
        }
        if self.packets.incoming.len() == QUEUE {
            return Err(Error::Capacity);
        }
        self.packets.incoming.push_back(bytes.to_vec());
        Ok(())
    }
    pub fn poll(&mut self, now_ms: u64) -> Result<(), Error> {
        if self.aborted {
            return Err(Error::State);
        }
        // smoltcp stores microseconds in a signed integer.
        let now = i64::try_from(now_ms)
            .ok()
            .filter(|n| *n <= i64::MAX / 1000 && *n >= self.last_ms)
            .ok_or(Error::Clock)?;
        self.last_ms = now;
        self.interface.poll(
            Instant::from_millis(now),
            &mut self.packets,
            &mut self.sockets,
        );
        Ok(())
    }
    pub fn take_packet(&mut self) -> Option<Vec<u8>> {
        self.packets.outgoing.pop_front()
    }
    pub fn state(&self) -> tcp::State {
        self.sockets.get::<tcp::Socket>(self.handle).state()
    }
    pub fn write(&mut self, bytes: &[u8]) -> Result<usize, Error> {
        if self.aborted {
            return Err(Error::State);
        }
        self.sockets
            .get_mut::<tcp::Socket>(self.handle)
            .send_slice(bytes)
            .map_err(|_| Error::State)
    }
    pub fn read(&mut self, bytes: &mut [u8]) -> Result<usize, Error> {
        if self.aborted {
            return Err(Error::State);
        }
        self.sockets
            .get_mut::<tcp::Socket>(self.handle)
            .recv_slice(bytes)
            .map_err(|_| Error::State)
    }
    pub(crate) fn read_io(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        use std::io::ErrorKind;
        if self.aborted {
            return Err(ErrorKind::NotConnected.into());
        }
        if bytes.is_empty() {
            return Ok(0);
        }
        match self
            .sockets
            .get_mut::<tcp::Socket>(self.handle)
            .recv_slice(bytes)
        {
            Ok(0) => Err(ErrorKind::WouldBlock.into()),
            Ok(n) => Ok(n),
            Err(tcp::RecvError::Finished) => Ok(0),
            Err(tcp::RecvError::InvalidState) => Err(ErrorKind::ConnectionReset.into()),
        }
    }
    pub(crate) fn write_io(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        use std::io::ErrorKind;
        if self.aborted {
            return Err(ErrorKind::NotConnected.into());
        }
        if bytes.is_empty() {
            return Ok(0);
        }
        match self
            .sockets
            .get_mut::<tcp::Socket>(self.handle)
            .send_slice(bytes)
        {
            Ok(0) => Err(ErrorKind::WouldBlock.into()),
            Ok(n) => Ok(n),
            Err(_) => Err(ErrorKind::BrokenPipe.into()),
        }
    }
    pub fn close(&mut self) {
        self.sockets.get_mut::<tcp::Socket>(self.handle).close();
    }
    /// Make buffered data inaccessible and discard packets on cancellation or revocation.
    pub fn abort(&mut self) {
        self.aborted = true;
        self.sockets.get_mut::<tcp::Socket>(self.handle).abort();
        self.packets.incoming.clear();
        self.packets.outgoing.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pair() -> (Stream, Stream) {
        let a = "100.64.0.1:49152".parse().unwrap();
        let b = "100.64.0.2:80".parse().unwrap();
        (
            Stream::connect(a, b, 123).unwrap(),
            Stream::new(b, a, 456, true).unwrap(),
        )
    }
    fn exchange(a: &mut Stream, b: &mut Stream, now: u64) {
        a.poll(now).unwrap();
        while let Some(p) = a.take_packet() {
            b.ingest(&p).unwrap();
        }
        b.poll(now).unwrap();
        while let Some(p) = b.take_packet() {
            a.ingest(&p).unwrap();
        }
    }
    #[test]
    fn handshake_loss_transfer_and_close() {
        let (mut a, mut b) = pair();
        a.poll(0).unwrap();
        assert!(a.take_packet().is_some()); // Drop initial SYN.
        for now in (100..5000).step_by(100) {
            exchange(&mut a, &mut b, now);
        }
        assert_eq!(a.state(), tcp::State::Established);
        assert_eq!(b.state(), tcp::State::Established);
        assert_eq!(a.write(b"request").unwrap(), 7);
        for now in (5000..6000).step_by(100) {
            exchange(&mut a, &mut b, now);
        }
        let mut data = [0; 32];
        assert_eq!(b.read(&mut data).unwrap(), 7);
        assert_eq!(&data[..7], b"request");
        b.write(b"response").unwrap();
        b.close();
        for now in (6000..7000).step_by(100) {
            exchange(&mut a, &mut b, now);
        }
        assert_eq!(a.read(&mut data).unwrap(), 8);
        assert_eq!(&data[..8], b"response");
        assert_eq!(a.state(), tcp::State::CloseWait);
    }
    #[test]
    fn bounded_ingress_and_clock() {
        let (mut a, mut b) = pair();
        a.poll(1).unwrap();
        let packet = a.take_packet().unwrap();
        for _ in 0..QUEUE {
            b.ingest(&packet).unwrap();
        }
        assert!(matches!(b.ingest(&packet), Err(Error::Capacity)));
        assert!(matches!(a.ingest(&packet), Err(Error::Packet)));
        assert!(matches!(a.poll(0), Err(Error::Clock)));
        assert!(matches!(a.poll(u64::MAX), Err(Error::Clock)));
        b.abort();
        assert!(b.take_packet().is_none());
    }
    #[test]
    fn data_retransmission_backpressure_and_abort() {
        let (mut a, mut b) = pair();
        for now in 0..10 {
            exchange(&mut a, &mut b, now);
        }
        assert_eq!(a.write(&vec![42; BUFFER + 1]).unwrap(), BUFFER);
        assert_eq!(a.write(b"overflow").unwrap(), 0);
        a.poll(10).unwrap();
        assert!(a.take_packet().is_some()); // Drop first data segment.
        while let Some(p) = a.take_packet() {
            b.ingest(&p).unwrap();
        }
        let mut received = Vec::new();
        for now in (20..15000).step_by(20) {
            exchange(&mut a, &mut b, now);
            let mut bytes = [0; 2048];
            if let Ok(n) = b.read(&mut bytes) {
                received.extend_from_slice(&bytes[..n]);
            }
        }
        assert_eq!(received, vec![42; BUFFER]);
        a.write(b"buffered before abort").unwrap();
        for now in 15000..15010 {
            exchange(&mut a, &mut b, now);
        }
        b.abort();
        assert!(matches!(b.read(&mut [0; 32]), Err(Error::State)));
        assert!(matches!(b.write(b"late"), Err(Error::State)));
        assert!(matches!(b.poll(16000), Err(Error::State)));
        assert!(matches!(b.ingest(&[]), Err(Error::State)));
        assert!(b.take_packet().is_none());
    }
    #[test]
    fn unanswered_connection_times_out() {
        let (mut a, _) = pair();
        for now in (0..32000).step_by(100) {
            a.poll(now).unwrap();
            while a.take_packet().is_some() {}
        }
        assert_eq!(a.state(), tcp::State::Closed);
    }
    #[test]
    fn malformed_packets_never_enter_queue() {
        let (mut a, mut b) = pair();
        a.poll(0).unwrap();
        let packet = a.take_packet().unwrap();
        for length in 0..packet.len() {
            assert!(matches!(b.ingest(&packet[..length]), Err(Error::Packet)));
        }
        for index in [10, 20, 22, 36] {
            let mut broken = packet.clone();
            broken[index] ^= 1;
            assert!(matches!(b.ingest(&broken), Err(Error::Packet)));
        }
        let mut extra = packet.clone();
        extra.push(0);
        assert!(matches!(b.ingest(&extra), Err(Error::Packet)));
        let mut fragmented = packet.clone();
        let mut ip = Ipv4Packet::new_unchecked(&mut fragmented[..]);
        ip.set_more_frags(true);
        ip.fill_checksum();
        assert!(matches!(b.ingest(&fragmented), Err(Error::Packet)));
        assert!(b.packets.incoming.is_empty());
        b.ingest(&packet).unwrap();
    }
}
