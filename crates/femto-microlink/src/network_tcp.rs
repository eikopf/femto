use super::*;
use crate::tcp::{Socket, Stream};
use std::{net::SocketAddrV4, sync::Arc};

pub(super) struct Connection {
    pub(super) permit: HandshakePermit,
    identity: Arc<()>,
    peer: Key,
    local: SocketAddrV4,
    remote: SocketAddrV4,
    started_ms: u64,
    stream: Stream,
    reply_lease: Option<Arc<()>>,
    reply_armed: bool,
}
pub(super) fn matches(connections: &BTreeMap<u64, Connection>, socket: &Socket) -> bool {
    connections
        .get(&socket.id)
        .is_some_and(|c| Arc::ptr_eq(&c.identity, &socket.identity))
}
impl<S: EpochStore> Network<S> {
    /// Allocate one of four outbound TCP flows with exact-tuple reply permission.
    /// Ephemeral ports are not reused during this owner's lifetime.
    pub fn connect_tcp(
        &mut self,
        peer: Key,
        remote: SocketAddrV4,
        now_ms: u64,
    ) -> Result<Socket, Error> {
        let address = self
            .local_address(now_ms)?
            .ok_or(crate::tcp::Error::State)?;
        if self.tcp_connections.len() >= 4 || self.tcp_port > u32::from(u16::MAX) {
            return Err(Error::Capacity);
        }
        let permit = self
            .coordinator
            .data_plane()
            .begin_handshake(peer, now_ms)?;
        let local = SocketAddrV4::new(address, self.tcp_port as u16);
        // Validate the endpoint against the authenticated peer directory before allocation.
        self.coordinator
            .data_plane()
            .validate_remote_address(peer, *remote.ip(), now_ms)?;
        let mut random = [0; 8];
        getrandom::getrandom(&mut random).map_err(|e| io::Error::other(e.to_string()))?;
        let stream = Stream::connect(local, remote, u64::from_le_bytes(random))?;
        let id = self.tcp_next.checked_add(1).ok_or(Error::Capacity)?;
        let identity = Arc::new(());
        self.tcp_connections.insert(
            id,
            Connection {
                permit,
                identity: identity.clone(),
                peer,
                local,
                remote,
                started_ms: now_ms,
                stream,
                reply_lease: Some(Arc::new(())),
                reply_armed: false,
            },
        );
        self.tcp_next = id;
        self.tcp_port += 1;
        Ok(Socket { id, identity })
    }
    fn tcp_connection(&mut self, socket: &Socket, now_ms: u64) -> Result<&mut Connection, Error> {
        self.tick(now_ms)?;
        if !matches(&self.tcp_connections, socket) {
            return Err(crate::tcp::Error::State.into());
        }
        self.tcp_connections
            .get_mut(&socket.id)
            .ok_or(crate::tcp::Error::State.into())
    }
    pub fn tcp_local_address(
        &mut self,
        socket: &Socket,
        now_ms: u64,
    ) -> Result<SocketAddrV4, Error> {
        Ok(self.tcp_connection(socket, now_ms)?.local)
    }
    pub fn tcp_state(
        &mut self,
        socket: &Socket,
        now_ms: u64,
    ) -> Result<smoltcp::socket::tcp::State, Error> {
        Ok(self.tcp_connection(socket, now_ms)?.stream.state())
    }
    pub fn write_tcp(
        &mut self,
        socket: &Socket,
        bytes: &[u8],
        now_ms: u64,
    ) -> Result<usize, Error> {
        Ok(self.tcp_connection(socket, now_ms)?.stream.write(bytes)?)
    }
    pub fn read_tcp(
        &mut self,
        socket: &Socket,
        bytes: &mut [u8],
        now_ms: u64,
    ) -> Result<usize, Error> {
        Ok(self.tcp_connection(socket, now_ms)?.stream.read(bytes)?)
    }
    pub fn read_tcp_io(
        &mut self,
        socket: &Socket,
        bytes: &mut [u8],
        now_ms: u64,
    ) -> Result<usize, Error> {
        Ok(self.tcp_connection(socket, now_ms)?.stream.read_io(bytes)?)
    }
    pub fn write_tcp_io(
        &mut self,
        socket: &Socket,
        bytes: &[u8],
        now_ms: u64,
    ) -> Result<usize, Error> {
        Ok(self
            .tcp_connection(socket, now_ms)?
            .stream
            .write_io(bytes)?)
    }
    pub fn close_tcp(&mut self, socket: &Socket, now_ms: u64) -> Result<(), Error> {
        self.tcp_connection(socket, now_ms)?.stream.close();
        Ok(())
    }
    /// Remove the flow, including ciphertext waiting for transport delivery.
    pub fn abort_tcp(&mut self, socket: &Socket) -> Result<(), Error> {
        if !matches(&self.tcp_connections, socket) {
            return Err(crate::tcp::Error::State.into());
        }
        self.tcp_connections.remove(&socket.id);
        self.outgoing.retain(|p| {
            p.tcp_socket
                .as_ref()
                .is_none_or(|s| !Arc::ptr_eq(&s.identity, &socket.identity))
        });
        Ok(())
    }
    /// Advance one flow. Call alongside the host or device network pump.
    /// A full ciphertext queue leaves remaining TCP packets in the stream engine.
    pub fn poll_tcp(&mut self, socket: &Socket, now_ms: u64) -> Result<(), Error> {
        let c = self.tcp_connection(socket, now_ms)?;
        let (peer, local, remote, started) = (c.peer, c.local, c.remote, c.started_ms);
        if !self.ensure_session(peer, now_ms)? {
            return Ok(());
        }
        for _ in 0..INCOMING {
            let Some(index) = self
                .incoming
                .iter()
                .position(|(key, p, _)| *key == peer && p.tcp_endpoints() == Some((remote, local)))
            else {
                break;
            };
            let (_, packet, _) = self.incoming.remove(index).ok_or(Error::Packet)?;
            let bytes = self.coordinator.data_plane().release(packet, now_ms)?;
            match self.tcp_connection(socket, now_ms)?.stream.ingest(&bytes) {
                Ok(()) => (),
                // TCP retransmits dropped packets. Malformed packets never reach its state machine.
                Err(crate::tcp::Error::Capacity | crate::tcp::Error::Packet) => (),
                Err(error) => return Err(error.into()),
            }
        }
        self.tcp_connection(socket, now_ms)?
            .stream
            .poll(now_ms - started)?;
        if self.tcp_connection(socket, now_ms)?.stream.state()
            == smoltcp::socket::tcp::State::Closed
        {
            self.tcp_connection(socket, now_ms)?.reply_lease = None;
        }
        for _ in 0..8 {
            if self.outgoing.len() >= OUTGOING {
                break;
            }
            let Some(bytes) = self.tcp_connection(socket, now_ms)?.stream.take_packet() else {
                break;
            };
            self.send_ip(peer, &bytes, now_ms)?;
            self.outgoing.back_mut().ok_or(Error::Packet)?.tcp_socket = Some(socket.clone());
            let connection = self.tcp_connection(socket, now_ms)?;
            connection.reply_armed = true;
            if let Some(lease) = connection.reply_lease.clone() {
                let permit = connection.permit.clone();
                self.coordinator
                    .data_plane()
                    .allow_tcp_reply(&permit, local, remote, &lease, now_ms)?;
            }
        }
        Ok(())
    }
}

/// Called inside map application only when peer authorization and policy are unchanged.
pub(super) fn refresh_authorization<S: EpochStore>(
    network: &mut Network<S>,
    now_ms: u64,
) -> Result<(), Error> {
    for connection in network.tcp_connections.values_mut() {
        let plane = network.coordinator.data_plane();
        connection.permit = plane.begin_handshake(connection.peer, now_ms)?;
        if connection.reply_armed
            && let Some(lease) = &connection.reply_lease
        {
            plane.allow_tcp_reply(
                &connection.permit,
                connection.local,
                connection.remote,
                lease,
                now_ms,
            )?;
        }
    }
    Ok(())
}
