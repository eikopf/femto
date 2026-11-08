//! Host aliases for the portable relay pool and its verified Rustls adapter.
pub use crate::relay_pool::{Error, PoolStats};
use crate::{
    host_tls::{NonblockingTls, TlsStream},
    relay_pool::Transport,
};
pub type Connection = crate::relay_pool::Connection<NonblockingTls>;
pub type AuthorizedConnection = crate::relay_pool::AuthorizedConnection<NonblockingTls>;
pub type Pool = crate::relay_pool::Pool<NonblockingTls>;

impl Transport for NonblockingTls {
    type Blocking = TlsStream;
    fn from_verified(stream: TlsStream) -> std::io::Result<Self> {
        stream.into_nonblocking()
    }
    fn check(&mut self) -> std::io::Result<()> {
        self.check()
    }
    fn queue(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.queue(bytes)
    }
    fn flush_pending(&mut self) -> std::io::Result<bool> {
        self.flush_pending()
    }
    fn receive(&mut self, buffer: &mut [u8]) -> std::io::Result<Option<usize>> {
        self.receive(buffer)
    }
    fn close(&mut self) {
        self.close();
    }
}
impl AuthorizedConnection {
    /// Blocking setup belongs outside the control-worker lock.
    pub fn connect(
        connector: &crate::host_tls::TlsConnector,
        plan: crate::network::RelayPlan,
        address: std::net::SocketAddr,
        key: crate::protocol::key::PrivateKey<crate::protocol::key::Node>,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
        now_ms: u64,
    ) -> Result<Self, Error> {
        if key.public() != plan.local_node() {
            return Err(Error::Closed);
        }
        let session = connector.connect_derp(plan.node(), address, key, cancelled)?;
        Self::from_session(session, plan, now_ms)
    }
}
