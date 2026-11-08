//! Host connector for the portable background relay worker.
pub use crate::relay_worker::{Progress, addresses};
use crate::{host_derp::AuthorizedConnection, host_tls::TlsConnector};
use std::{io, sync::atomic::Ordering, time::Duration};
pub type Dialer = crate::relay_worker::Dialer<crate::host_tls::NonblockingTls>;
impl Dialer {
    pub fn start(
        &mut self,
        plan: crate::network::RelayPlan,
        key: crate::protocol::key::PrivateKey<crate::protocol::key::Node>,
        now_ms: u64,
    ) -> io::Result<bool> {
        self.start_with_tls(
            plan,
            key,
            now_ms,
            TlsConnector::mozilla(Duration::from_secs(5))?,
        )
    }
    pub fn start_with_tls(
        &mut self,
        plan: crate::network::RelayPlan,
        key: crate::protocol::key::PrivateKey<crate::protocol::key::Node>,
        now_ms: u64,
        connector: TlsConnector,
    ) -> io::Result<bool> {
        let connector = connector.with_timeout(Duration::from_secs(5))?;
        self.start_with(
            plan,
            key,
            now_ms,
            2 * 1024 * 1024,
            move |plan, key, cancelled| {
                let addresses = addresses(plan.node())?;
                let mut last = crate::host_derp::Error::Closed;
                for address in addresses.into_iter().take(2) {
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    match AuthorizedConnection::connect(
                        &connector,
                        plan.clone(),
                        address,
                        key.clone(),
                        cancelled.clone(),
                        now_ms,
                    ) {
                        Ok(connection) => return Ok(connection),
                        Err(error) => last = error,
                    }
                }
                Err(last)
            },
        )
    }
}
