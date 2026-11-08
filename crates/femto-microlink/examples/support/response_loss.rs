/// Independent-peer fault fixture; production UDP polling never discards responses.
pub fn pump_drop_first_response<S: femto_microlink::timestamp::EpochStore>(
    owner: &mut femto_microlink::control_worker::ControlledNetwork<S>,
    udp: &femto_microlink::socket_udp::UdpTransport,
    clock: femto_microlink::coordinator::Clock,
    dropped: &mut Option<u64>,
) -> Result<femto_microlink::socket_network::PollStats, femto_microlink::control_worker::Error> {
    use femto_microlink::{
        discovery::Route,
        network::{self, Source},
        socket_network::PollStats,
    };
    owner.run(
        || clock,
        |network| {
            let now = clock.monotonic_ms;
            network.tick(now)?;
            let mut stats = PollStats::default();
            for _ in 0..8 {
                let Some(datagram) = udp.receive()? else {
                    break;
                };
                stats.received += 1;
                if dropped.is_none()
                    && datagram.bytes.len() == 92
                    && datagram.bytes.starts_with(&2u32.to_le_bytes())
                {
                    *dropped = Some(now);
                    continue;
                }
                match network.receive(Source::Udp(datagram.source), &datagram.bytes, now) {
                    Ok(()) => {
                        if datagram.bytes.starts_with(&2u32.to_le_bytes()) {
                            stats.handshake_responses += 1;
                        }
                    }
                    Err(error @ network::Error::Clock) => return Err(error),
                    Err(_) => stats.rejected += 1,
                }
            }
            stats.probes_queued = usize::from(network.schedule_probe(now)?);
            stats.stun_sent =
                usize::from(network.send_stun(now, |server, bytes| udp.send(server, bytes))?);
            for _ in 0..8 {
                stats.sent +=
                    usize::from(network.flush_one(now, |route, _, bytes| match route {
                        Route::Direct(endpoint) => udp.send(endpoint, bytes),
                        Route::Relay(_) => Err(std::io::ErrorKind::WouldBlock.into()),
                    })?);
            }
            Ok(stats)
        },
    )
}

/// Holds the verified relay until one response is discarded, then returns it
/// to the ordinary pool without reconnecting or changing network ownership.
#[derive(Default)]
pub struct RelayResponseLoss {
    connection: Option<femto_microlink::host_derp::AuthorizedConnection>,
    pub dropped_at: Option<u64>,
}
impl RelayResponseLoss {
    pub fn pump<S: femto_microlink::timestamp::EpochStore>(
        &mut self,
        owner: &mut femto_microlink::control_worker::ControlledNetwork<S>,
        pool: &mut femto_microlink::host_derp::Pool,
        connector: &femto_microlink::host_tls::TlsConnector,
        address: std::net::SocketAddr,
        key: &femto_microlink::protocol::key::PrivateKey<femto_microlink::protocol::key::Node>,
        clock: impl Fn() -> femto_microlink::coordinator::Clock + Copy,
    ) -> Result<(usize, usize, usize), Box<dyn std::error::Error>> {
        use femto_microlink::{network::Source, protocol::derp::Event};
        use std::sync::{Arc, atomic::AtomicBool};
        if self.dropped_at.is_some() {
            return super::fixture_transport::pump_fixture_relay(
                owner, pool, connector, address, key, clock,
            );
        }
        if self.connection.is_none() {
            let now = clock();
            let plan = owner.run(|| now, |network| pool.next_plan(network, now.monotonic_ms))?;
            let Some(plan) = plan else {
                return Ok((0, 0, 0));
            };
            self.connection = Some(femto_microlink::host_derp::AuthorizedConnection::connect(
                connector,
                plan,
                address,
                key.clone(),
                Arc::new(AtomicBool::new(false)),
                now.monotonic_ms,
            )?);
        }
        let stats = owner.run(clock, |network| {
            let now = clock().monotonic_ms;
            network.tick(now)?;
            let connection = self.connection.as_mut().expect("fixture relay");
            let mut events = 0;
            if let Some(event) = connection
                .poll(network, now)
                .map_err(std::io::Error::other)?
            {
                events += 1;
                match event {
                    Event::Packet { source, payload } => {
                        if payload.len() == 92 && payload.starts_with(&2u32.to_le_bytes()) {
                            self.dropped_at = Some(now);
                        } else {
                            network.receive(Source::Relay(source), &payload, now)?;
                        }
                    }
                    Event::Health(message) if !message.is_empty() => {
                        return Err(std::io::Error::other(
                            "relay health failure during loss fixture",
                        )
                        .into());
                    }
                    Event::Restarting { .. } => {
                        return Err(
                            std::io::Error::other("relay restarted during loss fixture").into()
                        );
                    }
                    _ => (),
                }
            }
            let mut sent = 0;
            for _ in 0..8 {
                sent += usize::from(
                    connection
                        .flush_one(network, now, |_, _| {
                            Err(std::io::ErrorKind::WouldBlock.into())
                        })
                        .map_err(std::io::Error::other)?,
                );
            }
            if self.dropped_at.is_some() {
                pool.insert(self.connection.take().expect("fixture relay"), network, now)
                    .map_err(std::io::Error::other)?;
            }
            Ok((events, sent, 0))
        })?;
        Ok(stats)
    }
}
