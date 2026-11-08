//! Host network driver and compatibility exports for portable application TCP.
pub use crate::application_tcp::{Driver, Shared, Stream};
use crate::{
    network::{self, Network},
    timestamp::EpochStore,
};
use std::io;
/// Host runtime with coordinator serialization and the existing UDP/DERP pump.
/// Relay connections must be configured by the caller through the existing pool API.
pub struct NetworkDriver<S: EpochStore, C: FnMut() -> crate::coordinator::Clock> {
    pub owner: crate::host_control_worker::ControlledNetwork<S>,
    pub udp: crate::host_udp::UdpTransport,
    pub relays: crate::host_derp::Pool,
    pub clock: C,
}
fn control_error(error: crate::host_control_worker::Error) -> io::Error {
    match error {
        crate::host_control_worker::Error::Network(network::Error::Io(error)) => error,
        crate::host_control_worker::Error::Spawn(error) => error,
        error => io::Error::new(io::ErrorKind::ConnectionAborted, error),
    }
}
impl<S: EpochStore, C: FnMut() -> crate::coordinator::Clock> Driver for NetworkDriver<S, C> {
    type Store = S;
    fn run<R>(
        &mut self,
        operation: impl FnOnce(&mut Network<S>, u64) -> Result<R, network::Error>,
    ) -> io::Result<R> {
        let clock = (self.clock)();
        self.owner
            .run(|| clock, |network| operation(network, clock.monotonic_ms))
            .map_err(control_error)
    }
    fn pump(&mut self) -> io::Result<()> {
        crate::host_network::poll_with_relays(
            &mut self.owner,
            &self.udp,
            &mut self.relays,
            (self.clock)(),
        )
        .map(|_| ())
        .map_err(control_error)
    }
}
