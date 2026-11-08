//! One bounded UDP request with peer-name binding and an overall deadline.
use crate::{
    network::{self, Network},
    protocol::key::{Node, PublicKey},
    timestamp::EpochStore,
    udp,
};
use std::net::SocketAddrV4;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid UDP request name, port, payload, or timeout")]
    Invalid,
    #[error("UDP request deadline elapsed")]
    Timeout,
    #[error("UDP request clock moved backwards")]
    Clock,
    #[error("UDP request peer or assigned address became unavailable or changed")]
    Changed,
    #[error("UDP request is complete or cancelled")]
    Finished,
    #[error(transparent)]
    Network(#[from] network::Error),
}
pub struct Request {
    name: String,
    port: u16,
    local_port: u16,
    payload: Vec<u8>,
    deadline: u64,
    last_ms: u64,
    target: Option<(PublicKey<Node>, SocketAddrV4)>,
    socket: Option<udp::Socket>,
    sent: bool,
    finished: bool,
}
impl Request {
    /// The timeout covers map readiness, name resolution, handshake, and reply.
    /// The payload is sent once after session readiness; no UDP retransmission.
    pub fn new(
        name: &str,
        port: u16,
        local_port: u16,
        payload: &[u8],
        now_ms: u64,
        timeout_ms: u64,
    ) -> Result<Self, Error> {
        let name = crate::coordinator::dns_name(name)
            .map_err(|_| Error::Invalid)?
            .ok_or(Error::Invalid)?;
        if port == 0 || payload.len() > udp::MAX_PAYLOAD || !(1..=60000).contains(&timeout_ms) {
            return Err(Error::Invalid);
        }
        Ok(Self {
            name,
            port,
            local_port,
            payload: payload.to_vec(),
            deadline: now_ms.checked_add(timeout_ms).ok_or(Error::Invalid)?,
            last_ms: now_ms,
            target: None,
            socket: None,
            sent: false,
            finished: false,
        })
    }
    /// Call under the control worker's operation boundary while polling packet
    /// transports. None means pending; Some is the first matching response.
    pub fn poll<S: EpochStore>(
        &mut self,
        network: &mut Network<S>,
        now_ms: u64,
    ) -> Result<Option<Vec<u8>>, Error> {
        if self.finished {
            return Err(Error::Finished);
        }
        let result = self.step(network, now_ms);
        if !matches!(result, Ok(None)) {
            self.cancel(network);
        }
        result
    }
    fn step<S: EpochStore>(
        &mut self,
        network: &mut Network<S>,
        now_ms: u64,
    ) -> Result<Option<Vec<u8>>, Error> {
        if now_ms < self.last_ms {
            return Err(Error::Clock);
        }
        self.last_ms = now_ms;
        if now_ms >= self.deadline {
            return Err(Error::Timeout);
        }
        if network.local_address(now_ms)?.is_none() {
            return if self.target.is_some() {
                Err(Error::Changed)
            } else {
                Ok(None)
            };
        }
        let Some((peer, address)) = network.resolve(&self.name, now_ms)? else {
            return if self.target.is_some() {
                Err(Error::Changed)
            } else {
                Ok(None)
            };
        };
        let endpoint = SocketAddrV4::new(address, self.port);
        if self.target.is_some_and(|target| target != (peer, endpoint)) {
            return Err(Error::Changed);
        }
        self.target = Some((peer, endpoint));
        let socket = match self.socket {
            Some(socket) => socket,
            None => {
                let socket = network.bind_udp(self.local_port, now_ms)?;
                self.socket = Some(socket);
                socket
            }
        };
        if !self.sent {
            self.sent = match network.try_send_udp(socket, peer, endpoint, &self.payload, now_ms) {
                Ok(sent) => sent,
                Err(
                    network::Error::Capacity
                    | network::Error::Handshake(crate::handshake::Error::Capacity),
                ) => false,
                Err(error) => return Err(error.into()),
            };
            if !self.sent {
                return Ok(None);
            }
        }
        for _ in 0..8 {
            let Some(packet) = network.receive_udp(socket, now_ms)? else {
                break;
            };
            if packet.peer == peer && packet.source == endpoint {
                return Ok(Some(packet.payload));
            }
        }
        Ok(None)
    }
    /// Also removes queued output and input belonging to this request's socket.
    /// Call before dropping an active request if its network owner will survive.
    pub fn cancel<S: EpochStore>(&mut self, network: &mut Network<S>) {
        if let Some(socket) = self.socket.take() {
            let _ = network.close_udp(socket);
        }
        self.payload.clear();
        self.finished = true;
    }
}
