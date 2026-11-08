//! Nonblocking device TLS adapter for the shared authorized relay pool.
use femto_catalog::http::Stream;
use femto_microlink::relay_pool::Transport;
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub struct Tls {
    stream: Option<Box<dyn Stream + Send>>,
    cancelled: Arc<AtomicBool>,
}
fn pending(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}
impl Transport for Tls {
    type Blocking = crate::network::Connection;
    fn from_verified(stream: Self::Blocking) -> io::Result<Self> {
        crate::clock::require_ready()?;
        let (stream, cancelled) = stream.into_nonblocking()?;
        Ok(Self {
            stream: Some(stream),
            cancelled,
        })
    }
    fn check(&mut self) -> io::Result<()> {
        let result = if self.stream.is_none() || self.cancelled.load(Ordering::Acquire) {
            Err(io::ErrorKind::ConnectionAborted.into())
        } else {
            crate::clock::require_ready()
        };
        if result.is_err() {
            self.close();
        }
        result
    }
    fn queue(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.check()?;
        let result = self.stream.as_mut().unwrap().write(bytes);
        self.check()?;
        match result {
            Err(error) if pending(&error) => Ok(0),
            Ok(0) if !bytes.is_empty() => {
                self.close();
                Err(io::ErrorKind::WriteZero.into())
            }
            Err(error) => {
                self.close();
                Err(error)
            }
            result => result,
        }
    }
    fn flush_pending(&mut self) -> io::Result<bool> {
        self.check()?;
        let result = self.stream.as_mut().unwrap().flush();
        self.check()?;
        match result {
            Ok(()) => Ok(true),
            Err(error) if pending(&error) => Ok(false),
            Err(error) => {
                self.close();
                Err(error)
            }
        }
    }
    fn receive(&mut self, output: &mut [u8]) -> io::Result<Option<usize>> {
        self.check()?;
        let mut staged = [0; 1024];
        let length = output.len().min(staged.len());
        let result = self.stream.as_mut().unwrap().read(&mut staged[..length]);
        self.check()?;
        match result {
            Ok(count) => {
                if count == 0 && length != 0 {
                    self.close();
                }
                output[..count].copy_from_slice(&staged[..count]);
                Ok(Some(count))
            }
            Err(error) if pending(&error) => Ok(None),
            Err(error) => {
                self.close();
                Err(error)
            }
        }
    }
    fn close(&mut self) {
        self.stream = None;
    }
}

pub type Pool = femto_microlink::relay_pool::Pool<Tls>;
pub type AuthorizedConnection = femto_microlink::relay_pool::AuthorizedConnection<Tls>;

/// Performs blocking setup outside the network-owner lock. The returned
/// connection must be inserted under that lock before polling or packet release.
pub fn connect(
    plan: femto_microlink::network::RelayPlan,
    address: std::net::SocketAddr,
    key: femto_microlink::protocol::key::PrivateKey<femto_microlink::protocol::key::Node>,
    timeout: std::time::Duration,
    cancelled: Arc<AtomicBool>,
    now_ms: u64,
) -> Result<AuthorizedConnection, femto_microlink::relay_pool::Error> {
    if key.public() != plan.local_node() {
        return Err(femto_microlink::relay_pool::Error::Closed);
    }
    let session = crate::relay::connect(plan.node(), address, key, timeout, cancelled)?;
    AuthorizedConnection::from_session(session, plan, now_ms)
}
