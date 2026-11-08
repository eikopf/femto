//! Shared direct or relay-only transport for independent host fixtures.
pub fn pump_fixture_relay<S: femto_microlink::timestamp::EpochStore>(
    owner: &mut femto_microlink::control_worker::ControlledNetwork<S>,
    pool: &mut femto_microlink::host_derp::Pool,
    connector: &femto_microlink::host_tls::TlsConnector,
    address: std::net::SocketAddr,
    key: &femto_microlink::protocol::key::PrivateKey<femto_microlink::protocol::key::Node>,
    clock: impl Fn() -> femto_microlink::coordinator::Clock + Copy,
) -> Result<(usize, usize, usize), Box<dyn std::error::Error>> {
    use std::sync::{Arc, atomic::AtomicBool};
    let now = clock();
    let plan = owner.run(|| now, |network| pool.next_plan(network, now.monotonic_ms))?;
    if let Some(plan) = plan {
        let connection = femto_microlink::host_derp::AuthorizedConnection::connect(
            connector,
            plan,
            address,
            key.clone(),
            Arc::new(AtomicBool::new(false)),
            now.monotonic_ms,
        )?;
        owner.run(clock, |network| {
            pool.insert(connection, network, clock().monotonic_ms)
                .map_err(|error| std::io::Error::other(error).into())
        })?;
    }
    Ok(owner.run(clock, |network| {
        let now = clock().monotonic_ms;
        network.tick(now)?;
        let stats = pool.poll(network, now)?;
        let mut sent = 0;
        for _ in 0..8 {
            sent += usize::from(pool.flush_one(network, now, |_, _| {
                Err(std::io::ErrorKind::WouldBlock.into())
            })?);
        }
        Ok((stats.events, sent, stats.handshake_responses))
    })?)
}

/// Fixture transport keeps relay setup outside the blocking application's pump.
pub struct FixtureDriver<S: femto_microlink::timestamp::EpochStore, C> {
    pub owner: femto_microlink::control_worker::ControlledNetwork<S>,
    pub udp: Option<femto_microlink::socket_udp::UdpTransport>,
    pub relays: femto_microlink::host_derp::Pool,
    pub clock: C,
    pub direct_sent: usize,
    pub relay_events: usize,
}
impl<S: femto_microlink::timestamp::EpochStore, C: Fn() -> femto_microlink::coordinator::Clock>
    femto_microlink::application_tcp::Driver for FixtureDriver<S, C>
{
    type Store = S;
    fn run<R>(
        &mut self,
        operation: impl FnOnce(
            &mut femto_microlink::network::Network<S>,
            u64,
        ) -> Result<R, femto_microlink::network::Error>,
    ) -> std::io::Result<R> {
        let now = (self.clock)();
        self.owner
            .run(|| now, |network| operation(network, now.monotonic_ms))
            .map_err(fixture_control_error)
    }
    fn pump(&mut self) -> std::io::Result<()> {
        let now = (self.clock)();
        if let Some(udp) = &self.udp {
            let stats = femto_microlink::socket_network::poll_with_relays(
                &mut self.owner,
                udp,
                &mut self.relays,
                now,
            )
            .map_err(fixture_control_error)?;
            self.direct_sent += stats.sent;
            self.relay_events += stats.relay_events;
        } else {
            let events = self
                .owner
                .run(
                    || now,
                    |network| {
                        network.tick(now.monotonic_ms)?;
                        let stats = self.relays.poll(network, now.monotonic_ms)?;
                        for _ in 0..8 {
                            self.relays.flush_one(network, now.monotonic_ms, |_, _| {
                                Err(std::io::ErrorKind::WouldBlock.into())
                            })?;
                        }
                        Ok(stats.events)
                    },
                )
                .map_err(fixture_control_error)?;
            self.relay_events += events;
        }
        Ok(())
    }
}
fn fixture_control_error(error: femto_microlink::control_worker::Error) -> std::io::Error {
    match error {
        femto_microlink::control_worker::Error::Network(femto_microlink::network::Error::Io(
            error,
        )) => error,
        error => std::io::Error::new(std::io::ErrorKind::ConnectionAborted, error),
    }
}
