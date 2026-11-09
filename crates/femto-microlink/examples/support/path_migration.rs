//! Independent-peer fixture: fail direct I/O for one interval, retaining TCP.
use femto_microlink::{
    control_worker::ControlledNetwork,
    coordinator::Clock,
    host_derp::{AuthorizedConnection, Pool},
    host_tls::TlsConnector,
    network::{self, Source},
    protocol::key::{Node, PrivateKey},
    socket_network::PollStats,
    socket_udp::UdpTransport,
    timestamp::EpochStore,
};
use std::{
    io,
    net::SocketAddr,
    sync::{Arc, atomic::AtomicBool},
};

pub struct Pump<'a, S: EpochStore, C> {
    pub owner: &'a mut ControlledNetwork<S>,
    pub pool: &'a mut Pool,
    pub udp: &'a UdpTransport,
    pub connector: &'a TlsConnector,
    pub address: SocketAddr,
    pub key: &'a PrivateKey<Node>,
    pub clock: C,
}

#[derive(Default)]
pub struct Migration {
    direct_sent_total: usize,
    direct_data_sent: [usize; 3],
    direct_data_received: [usize; 3],
    relay_packets_queued: [usize; 3],
    relay_events: [usize; 3],
    outage_send_errors: usize,
    outage_datagrams_discarded: usize,
    read_at_outage: Option<usize>,
    read_at_recovery: Option<usize>,
}

impl Migration {
    pub fn direct_sent(&self) -> usize {
        self.direct_sent_total
    }

    pub fn pump<S: EpochStore, C: Fn() -> Clock + Copy>(
        &mut self,
        context: Pump<'_, S, C>,
        elapsed: Option<u64>,
        read: usize,
    ) -> Result<PollStats, Box<dyn std::error::Error>> {
        let Pump {
            owner,
            pool,
            udp,
            connector,
            address,
            key,
            clock,
        } = context;
        let phase = match elapsed {
            Some(40_000..100_000) => 1,
            Some(100_000..) => 2,
            _ => 0,
        };
        if phase == 1 {
            self.read_at_outage.get_or_insert(read);
        } else if phase == 2 {
            self.read_at_recovery.get_or_insert(read);
        }
        let now = clock();
        let plan = owner.run(|| now, |network| pool.next_plan(network, now.monotonic_ms))?;
        if let Some(plan) = plan {
            let connection = AuthorizedConnection::connect(
                connector,
                plan,
                address,
                key.clone(),
                Arc::new(AtomicBool::new(false)),
                now.monotonic_ms,
            )?;
            owner.run(clock, |network| {
                pool.insert(connection, network, clock().monotonic_ms)
                    .map_err(|error| io::Error::other(error).into())
            })?;
        }
        Ok(owner.run(clock, |network| {
            let now = clock().monotonic_ms;
            network.tick(now)?;
            let relay = pool.poll(network, now)?;
            self.relay_events[phase] += relay.events;
            let mut stats = PollStats {
                received: relay.events,
                handshake_responses: relay.handshake_responses,
                ..Default::default()
            };
            for _ in 0..8 {
                let Some(datagram) = udp.receive()? else {
                    break;
                };
                if phase == 1 {
                    self.outage_datagrams_discarded += 1;
                    continue;
                }
                stats.received += 1;
                match network.receive(Source::Udp(datagram.source), &datagram.bytes, now) {
                    Ok(()) => {
                        if datagram.bytes.get(..4) == Some(&2u32.to_le_bytes()) {
                            stats.handshake_responses += 1;
                        }
                        if is_data(&datagram.bytes) {
                            self.direct_data_received[phase] += 1;
                        }
                    }
                    Err(error @ network::Error::Clock) => return Err(error),
                    Err(_) => stats.rejected += 1,
                }
            }
            network.schedule_probe(now)?;
            for _ in 0..8 {
                let mut direct = false;
                let result = pool.flush_one(network, now, |endpoint, bytes| {
                    direct = true;
                    if phase == 1 {
                        self.outage_send_errors += 1;
                        return Err(io::ErrorKind::NetworkUnreachable.into());
                    }
                    udp.send(endpoint, bytes)?;
                    self.direct_sent_total += 1;
                    if is_data(bytes) {
                        self.direct_data_sent[phase] += 1;
                    }
                    Ok(())
                });
                match result {
                    Ok(true) => {
                        stats.sent += 1;
                        if !direct {
                            self.relay_packets_queued[phase] += 1;
                        }
                    }
                    Ok(false) => (),
                    Err(network::Error::Io(_)) => stats.send_errors += 1,
                    Err(error) => return Err(error),
                }
            }
            Ok(stats)
        })?)
    }

    pub fn report(&self, read: usize) -> Result<serde_json::Value, &'static str> {
        if self.direct_data_sent[0] == 0
            || self.direct_data_sent[2] == 0
            || self.direct_data_received[0] == 0
            || self.direct_data_received[2] == 0
            || self.direct_data_sent[1] != 0
            || self.direct_data_received[1] != 0
            || self.relay_packets_queued[1] == 0
            || self.relay_events[1] == 0
            || self.outage_send_errors == 0
            || !matches!((self.read_at_outage, self.read_at_recovery), (Some(a), Some(b)) if a < b && b < read)
        {
            return Err("direct/DERP/direct data-path or continuous application progress missing");
        }
        Ok(serde_json::json!({
            "direct_data_sent": self.direct_data_sent,
            "direct_data_received": self.direct_data_received,
            "relay_packets_queued": self.relay_packets_queued,
            "relay_events": self.relay_events,
            "outage_send_errors": self.outage_send_errors,
            "outage_datagrams_discarded": self.outage_datagrams_discarded,
            "read_at_outage": self.read_at_outage,
            "read_at_recovery": self.read_at_recovery,
            "outage_start_elapsed_ms": 40000, "outage_end_elapsed_ms": 100000,
            "fault": "local direct sends return NetworkUnreachable and incoming UDP is discarded",
        }))
    }
}

fn is_data(bytes: &[u8]) -> bool {
    bytes.len() > 32 && bytes.get(..4) == Some(&4u32.to_le_bytes())
}
