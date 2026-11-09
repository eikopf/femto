//! Bounded nonblocking UDP polling under the control worker's authorization lock.
use crate::{
    control_worker::{ControlledNetwork, Error},
    coordinator::Clock,
    discovery::Route,
    network::{self, Network, Source},
    socket_udp::UdpTransport,
    timestamp::EpochStore,
};
use std::io;

const RECEIVE_BUDGET: usize = 8;
pub(crate) const SEND_BUDGET: usize = 8;

#[derive(Default, Debug)]
pub struct EchoStats {
    pub queued: usize,
    pub dropped: usize,
}

/// Opt-in diagnostic service. The caller invokes `poll` through the same
/// control-worker operation boundary as all other application access.
pub struct UdpEcho {
    port: u16,
    socket: Option<crate::udp::Socket>,
}
impl UdpEcho {
    pub fn new(port: u16) -> Result<Self, crate::udp::Error> {
        if port == 0 {
            return Err(crate::udp::Error::Packet);
        }
        Ok(Self { port, socket: None })
    }
    /// Consumes at most eight authorized datagrams and queues identical replies.
    /// A full output queue drops the reply; no plaintext retry queue is retained.
    pub fn poll<S: EpochStore>(
        &mut self,
        network: &mut Network<S>,
        now: u64,
    ) -> Result<EchoStats, network::Error> {
        let mut stats = EchoStats::default();
        if network.local_address(now)?.is_none() {
            self.socket = None;
            return Ok(stats);
        }
        let socket = match self.socket {
            Some(socket) => socket,
            None => {
                let socket = network.bind_udp(self.port, now)?;
                self.socket = Some(socket);
                socket
            }
        };
        for _ in 0..RECEIVE_BUDGET {
            let Some(packet) = network.receive_udp(socket, now)? else {
                break;
            };
            match network.send_udp(socket, packet.peer, packet.source, &packet.payload, now) {
                Ok(()) => stats.queued += 1,
                Err(
                    network::Error::Capacity | network::Error::Plane(_) | network::Error::Udp(_),
                ) => stats.dropped += 1,
                Err(error) => return Err(error),
            }
        }
        Ok(stats)
    }
    pub fn close<S: EpochStore>(&mut self, network: &mut Network<S>) -> Result<(), network::Error> {
        if let Some(socket) = self.socket.take() {
            network.close_udp(socket)?;
        }
        Ok(())
    }
}

#[derive(Default, Debug)]
pub struct PollStats {
    /// Direct UDP handshake responses accepted by the authenticated owner.
    pub handshake_responses: usize,
    /// DERP handshake responses accepted by the authenticated owner.
    pub relay_handshake_responses: usize,
    pub received: usize,
    pub rejected: usize,
    pub sent: usize,
    pub send_errors: usize,
    pub probes_queued: usize,
    pub stun_sent: usize,
    pub relay_events: usize,
    pub relay_rejected: usize,
    pub relays_removed: usize,
}

/// Performs at most eight receives and eight queue flush attempts. Relay output
/// remains queued until a relay adapter is installed or the packet expires.
/// Callers must poll regularly and close the owner on terminal socket errors.
pub fn poll_udp<S: EpochStore>(
    owner: &mut ControlledNetwork<S>,
    udp: &UdpTransport,
    clock: Clock,
) -> Result<PollStats, Error> {
    owner.run(|| clock, |network| pump(network, udp, clock.monotonic_ms))
}

pub(crate) fn receive_and_probe<S: EpochStore>(
    network: &mut Network<S>,
    udp: &UdpTransport,
    now: u64,
    stats: &mut PollStats,
) -> Result<(), network::Error> {
    let mut buffer = [0; crate::derp::MAX_DATAGRAM + 1];
    for _ in 0..RECEIVE_BUDGET {
        let (source, length) = match udp.receive_into(&mut buffer) {
            Ok(Some(datagram)) => datagram,
            Ok(None) => break,
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                stats.received += 1;
                stats.rejected += 1;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        stats.received += 1;
        match network.receive(Source::Udp(source), &buffer[..length], now) {
            Ok(()) => {
                if buffer[..length].get(..4) == Some(&2u32.to_le_bytes()) {
                    stats.handshake_responses += 1;
                }
            }
            Err(error @ network::Error::Clock) => return Err(error),
            Err(_) => stats.rejected += 1,
        }
    }
    stats.probes_queued = usize::from(network.schedule_probe(now)?);
    match network.send_stun(now, |server, bytes| udp.send(server, bytes)) {
        Ok(sent) => stats.stun_sent = usize::from(sent),
        Err(network::Error::Io(_)) => stats.send_errors += 1,
        Err(error) => return Err(error),
    }
    Ok(())
}

fn pump<S: EpochStore>(
    network: &mut Network<S>,
    udp: &UdpTransport,
    now: u64,
) -> Result<PollStats, network::Error> {
    network.tick(now)?;
    let mut stats = PollStats::default();
    receive_and_probe(network, udp, now, &mut stats)?;
    for _ in 0..SEND_BUDGET {
        match network.flush_one(now, |route, _, bytes| match route {
            Route::Direct(endpoint) => udp.send(endpoint, bytes),
            Route::Relay(_) => Err(io::ErrorKind::WouldBlock.into()),
        }) {
            Ok(true) => stats.sent += 1,
            // A blocked relay packet rotates behind unrelated direct traffic.
            Ok(false) => (),
            Err(network::Error::Io(_)) => stats.send_errors += 1,
            Err(error) => return Err(error),
        }
    }
    Ok(stats)
}

pub fn poll_with_relays<S: EpochStore, T: crate::relay_pool::Transport>(
    owner: &mut ControlledNetwork<S>,
    udp: &UdpTransport,
    relays: &mut crate::relay_pool::Pool<T>,
    clock: Clock,
) -> Result<PollStats, Error> {
    poll_with_optional_udp(owner, Some(udp), relays, clock)
}
/// With no UDP transport, only authenticated DERP traffic is received or sent.
/// Direct discovery and STUN are also disabled; queued direct packets cannot leave.
pub fn poll_with_optional_udp<S: EpochStore, T: crate::relay_pool::Transport>(
    owner: &mut ControlledNetwork<S>,
    udp: Option<&UdpTransport>,
    relays: &mut crate::relay_pool::Pool<T>,
    clock: Clock,
) -> Result<PollStats, Error> {
    let result = owner.run(
        || clock,
        |network| {
            let now = clock.monotonic_ms;
            network.tick(now)?;
            let relay = relays.poll(network, now)?;
            let mut stats = PollStats {
                relay_events: relay.events,
                relay_handshake_responses: relay.handshake_responses,
                relay_rejected: relay.rejected_packets,
                relays_removed: relay.removed,
                ..PollStats::default()
            };
            if let Some(udp) = udp {
                receive_and_probe(network, udp, now, &mut stats)?;
            }
            for _ in 0..SEND_BUDGET {
                match relays.flush_one(network, now, |endpoint, bytes| {
                    udp.ok_or_else(|| io::Error::from(io::ErrorKind::WouldBlock))?
                        .send(endpoint, bytes)
                }) {
                    Ok(true) => stats.sent += 1,
                    Ok(false) => (),
                    Err(network::Error::Io(_)) => stats.send_errors += 1,
                    Err(error) => return Err(error),
                }
            }
            Ok(stats)
        },
    );
    if result.is_err() {
        relays.close();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        handshake::{Event, Handshakes},
        plane::{DataPlane, Peer},
        protocol::identity::DeviceIdentity,
        timestamp::TimestampClock,
    };
    use std::{
        net::UdpSocket,
        time::{Duration, Instant},
    };

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
    fn network(identity: &DeviceIdentity) -> Network<Store> {
        Network::new(identity, TimestampClock::open(Store, 100).unwrap(), 4, 2).unwrap()
    }
    fn socket() -> UdpTransport {
        UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap()
    }
    #[test]
    fn bounded_receive_rejects_oversize_and_keeps_servicing_socket() {
        let mut network = network(&DeviceIdentity::generate().unwrap());
        let target = socket();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        for _ in 0..12 {
            sender
                .send_to(&[0; 2048], target.local_addr().unwrap())
                .unwrap();
        }
        let first = pump(&mut network, &target, 0).unwrap();
        assert_eq!((first.received, first.rejected, first.sent), (8, 8, 0));
        let second = pump(&mut network, &target, 1).unwrap();
        assert_eq!((second.received, second.rejected, second.sent), (4, 4, 0));
        assert_eq!(pump(&mut network, &target, 2).unwrap().received, 0);
        let mut forged_response = [0; 92];
        forged_response[..4].copy_from_slice(&2u32.to_le_bytes());
        sender
            .send_to(&forged_response, target.local_addr().unwrap())
            .unwrap();
        let rejected = pump(&mut network, &target, 3).unwrap();
        assert_eq!((rejected.received, rejected.rejected), (1, 1));
        assert_eq!(rejected.handshake_responses, 0);
    }
    #[test]
    fn socket_dispatch_handshake_confirmation_and_disconnect() {
        let local = DeviceIdentity::generate().unwrap();
        let remote = DeviceIdentity::generate().unwrap();
        let local_key = local.node_key().public();
        let remote_key = remote.node_key().public();
        let mut network = network(&local);
        let map = serde_json::to_vec(&serde_json::json!({
            "Node": {"ID":1,"Key":local_key.to_string(),"Addresses":["100.64.0.1/32"],"MachineAuthorized":true},
            "Peers":[{"ID":2,"Key":remote_key.to_string(),"Addresses":["100.64.0.2/32"],"MachineAuthorized":true}],
            "PacketFilter":[]
        })).unwrap();
        network
            .apply_map(
                &map,
                Clock {
                    unix_ms: 1_735_689_600_000,
                    monotonic_ms: 0,
                },
            )
            .unwrap();
        let mut plane = DataPlane::new("100.64.0.2".parse().unwrap(), 1, 1, 0).unwrap();
        plane
            .apply_update(
                true,
                &[Peer {
                    key: local_key,
                    addresses: vec!["100.64.0.1".parse().unwrap()],
                    expires_ms: 10000,
                    jailed: false,
                }],
                &[],
                br#"{"PacketFilter":[]}"#,
                0,
            )
            .unwrap();
        let mut handshakes = Handshakes::new(remote.node_key().clone(), 1, 0).unwrap();
        let mut timestamps = TimestampClock::open(Store, 100).unwrap();
        let target = socket();
        let sender = socket();
        sender
            .send(
                target.local_addr().unwrap(),
                &handshakes
                    .begin(&mut plane, local_key, &mut timestamps, 0)
                    .unwrap(),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let response = loop {
            let stats = pump(&mut network, &target, 1).unwrap();
            assert_eq!(stats.rejected, 0);
            if let Some(response) = sender.receive().unwrap() {
                break response;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        };
        assert!(
            matches!(handshakes.handle(&mut plane, &response.bytes, 2).unwrap(), Event::Established(peer) if peer == local_key)
        );
        sender
            .send(
                target.local_addr().unwrap(),
                &plane.send(local_key, &[], 3).unwrap(),
            )
            .unwrap();
        let stats = pump(&mut network, &target, 3).unwrap();
        assert_eq!((stats.received, stats.rejected), (1, 0));
        network.disconnect();
        sender
            .send(
                target.local_addr().unwrap(),
                &plane.send(local_key, &[], 4).unwrap(),
            )
            .unwrap();
        let stats = pump(&mut network, &target, 4).unwrap();
        assert_eq!((stats.received, stats.rejected, stats.sent), (1, 1, 0));
        assert!(network.receive_ip(4).unwrap().is_none());
    }
}
