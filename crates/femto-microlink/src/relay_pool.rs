//! Portable bounded DERP polling after verified TLS and authenticated DERP setup.
use crate::{
    derp::RelaySession,
    derp_frames::{self, Decoder},
    protocol::{
        derp::Event,
        key::{Node, PublicKey},
    },
};
use std::collections::{BTreeMap, VecDeque};

/// Nonblocking I/O on a verified TLS connection. Each method must return
/// promptly without retry loops. Implementations retain cancellation checks.
/// Buffered plaintext and TLS records must survive conversion unchanged.
pub trait Transport {
    type Blocking: crate::derp::DeadlineIo;
    fn from_verified(stream: Self::Blocking) -> std::io::Result<Self>
    where
        Self: Sized;
    fn check(&mut self) -> std::io::Result<()>;
    /// Accepts a prefix; zero means backpressure. Must not exceed bytes.len().
    fn queue(&mut self, bytes: &[u8]) -> std::io::Result<usize>;
    /// Performs bounded I/O; true means all accepted output has been flushed.
    fn flush_pending(&mut self) -> std::io::Result<bool>;
    /// None means pending. Some(0) on a nonempty buffer means authenticated EOF.
    fn receive(&mut self, buffer: &mut [u8]) -> std::io::Result<Option<usize>>;
    /// Closes without flushing retained output. Closed transports cannot reopen.
    fn close(&mut self);
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Frame(#[from] derp_frames::Error),
    #[error(transparent)]
    Session(#[from] crate::derp::Error),
    #[error("relay output queue full")]
    Busy,
    #[error("relay connection closed or deadline elapsed")]
    Closed,
    #[error(transparent)]
    Network(#[from] crate::network::Error),
}

/// One relay connection checked against its network owner before socket I/O.
pub struct AuthorizedConnection<T: Transport> {
    connection: Connection<T>,
    plan: crate::network::RelayPlan,
    permits: Vec<crate::network::DeliveryPermit>,
}
impl<T: Transport> AuthorizedConnection<T> {
    /// Attaches an authenticated session for the plan's local node. The caller
    /// must obtain the session through the plan's verified relay endpoint.
    /// The owner revalidates the plan before any post-handshake socket I/O.
    pub fn from_session(
        mut session: RelaySession<T::Blocking>,
        plan: crate::network::RelayPlan,
        now_ms: u64,
    ) -> Result<Self, Error> {
        if session.local_node() != plan.local_node() {
            return Err(Error::Closed);
        }
        if plan.is_home() {
            session.note_preferred(true)?;
        }
        Ok(Self {
            connection: Connection::from_session(session, now_ms)?,
            plan,
            permits: Vec::new(),
        })
    }
    fn check<S: crate::timestamp::EpochStore>(
        &mut self,
        network: &mut crate::network::Network<S>,
        now_ms: u64,
    ) -> Result<(), Error> {
        let result = (|| {
            if !network.validate_relay_plan(&self.plan, now_ms)? {
                return Err(Error::Closed);
            }
            if !self.connection.pending_output() {
                self.permits.clear();
            }
            for permit in &self.permits {
                if !network.validate_delivery(permit, now_ms)? {
                    return Err(Error::Closed);
                }
            }
            self.connection.check(now_ms)?;
            Ok(())
        })();
        if result.is_err() {
            self.close();
        }
        result
    }
    /// Direct sends use a nonblocking callback; other relay regions stay queued.
    /// At most two delivery permits are retained until TLS output is flushed.
    pub fn flush_one<S: crate::timestamp::EpochStore>(
        &mut self,
        network: &mut crate::network::Network<S>,
        now_ms: u64,
        mut direct: impl FnMut(std::net::SocketAddr, &[u8]) -> std::io::Result<()>,
    ) -> Result<bool, Error> {
        self.check(network, now_ms)?;
        Ok(
            network.flush_one_authorized(now_ms, |route, peer, bytes, permit| match route {
                crate::discovery::Route::Direct(endpoint) => direct(endpoint, bytes),
                crate::discovery::Route::Relay(region) => {
                    if region != self.plan.region() {
                        return Err(std::io::ErrorKind::WouldBlock.into());
                    }
                    self.accept(peer, bytes, permit, now_ms)
                }
            })?,
        )
    }
    fn accept(
        &mut self,
        peer: PublicKey<Node>,
        bytes: &[u8],
        permit: &crate::network::DeliveryPermit,
        now_ms: u64,
    ) -> std::io::Result<()> {
        if self.permits.len() >= 2 {
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        match self.connection.send(peer, bytes, now_ms) {
            Ok(()) => {
                self.permits.push(permit.clone());
                Ok(())
            }
            Err(Error::Busy) => Err(std::io::ErrorKind::WouldBlock.into()),
            Err(error) => {
                self.close();
                Err(std::io::Error::other(error))
            }
        }
    }
    /// Packet events contain encrypted peer data. Deliver them to
    /// Network::receive inside the same control-worker operation boundary.
    pub fn poll<S: crate::timestamp::EpochStore>(
        &mut self,
        network: &mut crate::network::Network<S>,
        now_ms: u64,
    ) -> Result<Option<Event>, Error> {
        self.check(network, now_ms)?;
        let result = self.connection.poll(now_ms);
        if result.is_err() {
            self.close();
        }
        result
    }
    pub fn close(&mut self) {
        self.permits.clear();
        self.connection.close();
    }
}
impl<T: Transport> Drop for AuthorizedConnection<T> {
    fn drop(&mut self) {
        self.close();
    }
}

pub struct Pool<T: Transport> {
    connections: BTreeMap<u64, AuthorizedConnection<T>>,
    failures: Vec<(crate::network::RelayPlan, u64)>,
    prefer_peer: bool,
    restarts: Vec<Restart>,
}
struct Restart {
    plan: crate::network::RelayPlan,
    ready_ms: u64,
    until_ms: u64,
}
#[derive(Default, Debug)]
pub struct PoolStats {
    /// WireGuard handshake responses accepted by the authenticated network owner.
    pub handshake_responses: usize,
    pub events: usize,
    pub rejected_packets: usize,
    pub removed: usize,
}
impl<T: Transport> Default for Pool<T> {
    fn default() -> Self {
        Self {
            connections: BTreeMap::new(),
            failures: Vec::new(),
            prefer_peer: false,
            restarts: Vec::new(),
        }
    }
}
impl<T: Transport> Pool<T> {
    fn same_endpoint(a: &crate::network::RelayPlan, b: &crate::network::RelayPlan) -> bool {
        a.local_node() == b.local_node() && a.region() == b.region() && a.node() == b.node()
    }
    /// Failed setup yields the worker to other endpoints for thirty seconds.
    /// Stale or cancelled setup must not call this method.
    pub(crate) fn setup_failed(&mut self, plan: crate::network::RelayPlan, now_ms: u64) {
        let retry_ms =
            if self.restarts.iter().any(|restart| {
                Self::same_endpoint(&restart.plan, &plan) && now_ms < restart.until_ms
            }) {
                5000
            } else {
                30_000
            };
        self.defer(plan, now_ms, retry_ms);
    }
    fn defer(&mut self, plan: crate::network::RelayPlan, now_ms: u64, delay_ms: u64) {
        self.prefer_peer = plan.is_home();
        self.failures
            .retain(|(old, until)| *until > now_ms && !Self::same_endpoint(old, &plan));
        // The authenticated directory contains at most 256 nodes. Older map
        // descriptors can also remain here until their cooldown expires.
        if self.failures.len() == 256 {
            let index = self
                .failures
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, until))| *until)
                .unwrap()
                .0;
            self.failures.swap_remove(index);
        }
        self.failures.push((plan, now_ms.saturating_add(delay_ms)));
    }
    fn ready(&self, plan: &crate::network::RelayPlan, now_ms: u64) -> bool {
        !self
            .failures
            .iter()
            .any(|(old, until)| *until > now_ms && Self::same_endpoint(old, plan))
    }
    pub fn len(&self) -> usize {
        self.connections.len()
    }
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }
    pub fn contains_region(&self, region: u64) -> bool {
        self.connections.contains_key(&region)
    }
    /// Revalidates authorization before reporting an established home connection.
    pub fn home_region<S: crate::timestamp::EpochStore>(
        &mut self,
        network: &mut crate::network::Network<S>,
        now_ms: u64,
    ) -> Option<u64> {
        self.maintain(network, now_ms);
        self.connections
            .iter()
            .find_map(|(region, connection)| connection.plan.is_home().then_some(*region))
    }
    /// Whether a home connection may still appear: the directory offers a
    /// home relay and no setup of one is cooling down after a failure.
    pub fn home_expected<S: crate::timestamp::EpochStore>(
        &mut self,
        network: &mut crate::network::Network<S>,
        now_ms: u64,
    ) -> Result<bool, crate::network::Error> {
        self.failures.retain(|(_, until)| *until > now_ms);
        Ok(!self.failures.iter().any(|(plan, _)| plan.is_home())
            && network.offers_home_relay(now_ms)?)
    }
    /// Home setup has priority over peer destinations. An ordinary connection
    /// to the home's server is promoted in place.
    pub fn next_plan<S: crate::timestamp::EpochStore>(
        &mut self,
        network: &mut crate::network::Network<S>,
        now_ms: u64,
    ) -> Result<Option<crate::network::RelayPlan>, crate::network::Error> {
        network.tick(now_ms)?;
        self.failures.retain(|(_, until)| *until > now_ms);
        self.restarts.retain(|restart| {
            now_ms < restart.until_ms
                && network
                    .validate_relay_plan(&restart.plan, now_ms)
                    .unwrap_or(false)
        });
        let home_region = self.home_region(network, now_ms);
        for restart in &self.restarts {
            if now_ms >= restart.ready_ms
                && self.ready(&restart.plan, now_ms)
                && !self.contains_region(restart.plan.region())
                && (!restart.plan.is_home() || home_region.is_none())
                && (restart.plan.is_home() || self.len() < 4)
            {
                return Ok(Some(restart.plan.clone()));
            }
        }
        let home_reserved = self.restarts.iter().any(|restart| restart.plan.is_home());
        let home = if home_region.is_none() && !home_reserved {
            network.home_relay_matching(now_ms, |plan| self.ready(plan, now_ms))?
        } else {
            None
        };
        // An ordinary connection to the home's own server becomes the home
        // by saying so; a second connection would only repeat the setup.
        let home = match home {
            Some(plan) => match self.connections.get_mut(&plan.region()) {
                Some(existing) if Self::same_endpoint(&existing.plan, &plan) => {
                    if existing
                        .connection
                        .enqueue(derp_frames::note_preferred(true), now_ms)
                        .is_ok()
                    {
                        existing.plan = plan;
                        self.prefer_peer = false;
                        network.home_relay_connected(now_ms)?;
                    }
                    None
                }
                _ => Some(plan),
            },
            None => None,
        };
        if home.is_some() && !self.prefer_peer {
            return Ok(home);
        }
        if self.len() < 4 {
            for plan in network.pending_relay_plans(now_ms)? {
                if !self.contains_region(plan.region())
                    && !self
                        .restarts
                        .iter()
                        .any(|restart| restart.plan.region() == plan.region())
                {
                    for alternative in network.relay_alternatives(&plan, now_ms)? {
                        if self.ready(&alternative, now_ms) {
                            return Ok(Some(alternative));
                        }
                    }
                }
            }
        }
        Ok(home)
    }
    fn maintain<S: crate::timestamp::EpochStore>(
        &mut self,
        network: &mut crate::network::Network<S>,
        now_ms: u64,
    ) -> usize {
        let before = self.len();
        self.connections
            .retain(|_, connection| connection.check(network, now_ms).is_ok());
        before - self.len()
    }
    fn remove_failed<S: crate::timestamp::EpochStore>(
        &mut self,
        region: u64,
        network: &mut crate::network::Network<S>,
        now_ms: u64,
    ) {
        if let Some(mut connection) = self.connections.remove(&region) {
            connection.close();
            if network
                .validate_relay_plan(&connection.plan, now_ms)
                .unwrap_or(false)
            {
                self.setup_failed(connection.plan.clone(), now_ms);
            }
        }
    }
    fn restarting<S: crate::timestamp::EpochStore>(
        &mut self,
        region: u64,
        network: &mut crate::network::Network<S>,
        now_ms: u64,
        reconnect_ms: u32,
        try_ms: u32,
    ) {
        if let Some(mut connection) = self.connections.remove(&region) {
            connection.close();
            if network
                .validate_relay_plan(&connection.plan, now_ms)
                .unwrap_or(false)
            {
                let ready_ms = now_ms.saturating_add(u64::from(reconnect_ms.min(60_000)));
                let until_ms = ready_ms.saturating_add(u64::from(try_ms.clamp(1, 30_000)));
                self.restarts
                    .retain(|restart| restart.plan.region() != region && restart.until_ms > now_ms);
                if self.restarts.len() == 4 {
                    let index = self
                        .restarts
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, restart)| restart.until_ms)
                        .unwrap()
                        .0;
                    self.restarts.swap_remove(index);
                }
                self.defer(
                    connection.plan.clone(),
                    now_ms,
                    ready_ms.saturating_sub(now_ms),
                );
                self.restarts.push(Restart {
                    plan: connection.plan.clone(),
                    ready_ms,
                    until_ms,
                });
            }
        }
    }
    /// Attaches completed setup under the owner lock. Stale plans are rejected.
    /// Home setup may evict one ordinary region when all four slots are occupied.
    pub fn insert<S: crate::timestamp::EpochStore>(
        &mut self,
        mut connection: AuthorizedConnection<T>,
        network: &mut crate::network::Network<S>,
        now_ms: u64,
    ) -> Result<(), Error> {
        network.tick(now_ms)?;
        self.maintain(network, now_ms);
        connection.check(network, now_ms)?;
        let region = connection.plan.region();
        if connection.plan.is_home() {
            self.connections.retain(|_, old| !old.plan.is_home());
        }
        if self.len() >= 4 && !self.contains_region(region) {
            if connection.plan.is_home() {
                let evict = self
                    .connections
                    .iter()
                    .find_map(|(id, old)| (!old.plan.is_home()).then_some(*id));
                if let Some(id) = evict {
                    self.connections.remove(&id);
                } else {
                    return Err(Error::Busy);
                }
            } else {
                return Err(Error::Busy);
            }
        }
        self.failures
            .retain(|(old, _)| !Self::same_endpoint(old, &connection.plan));
        self.restarts
            .retain(|restart| restart.plan.region() != region);
        let home = connection.plan.is_home();
        self.connections.insert(region, connection);
        self.prefer_peer = false;
        if home {
            network.home_relay_connected(now_ms)?;
        }
        Ok(())
    }
    /// As many events from each region as a poll takes datagrams from the
    /// UDP socket, for at most four regions. Decrypted peer payloads remain in Network's opaque, policy-checked receive queue.
    pub fn poll<S: crate::timestamp::EpochStore>(
        &mut self,
        network: &mut crate::network::Network<S>,
        now_ms: u64,
    ) -> Result<PoolStats, crate::network::Error> {
        network.tick(now_ms)?;
        let mut stats = PoolStats {
            removed: self.maintain(network, now_ms),
            ..Default::default()
        };
        let regions: Vec<_> = self.connections.keys().copied().collect();
        for region in regions {
            for _ in 0..crate::socket_network::RECEIVE_BUDGET {
                let Some(connection) = self.connections.get_mut(&region) else {
                    break;
                };
                match connection.poll(network, now_ms) {
                    Ok(Some(event)) => {
                        stats.events += 1;
                        let remove = match event {
                            Event::Packet { source, payload } => {
                                if network
                                    .receive(
                                        crate::network::Source::Relay(source),
                                        &payload,
                                        now_ms,
                                    )
                                    .is_err()
                                {
                                    stats.rejected_packets += 1;
                                } else if payload.get(..4) == Some(&2u32.to_le_bytes()) {
                                    stats.handshake_responses += 1;
                                }
                                false
                            }
                            Event::Health(message) => !message.is_empty(),
                            Event::Restarting {
                                reconnect_in_ms,
                                try_for_ms,
                            } => {
                                self.restarting(
                                    region,
                                    network,
                                    now_ms,
                                    reconnect_in_ms,
                                    try_for_ms,
                                );
                                stats.removed += 1;
                                false
                            }
                            _ => false,
                        };
                        if remove {
                            self.remove_failed(region, network, now_ms);
                            stats.removed += 1;
                        }
                    }
                    Ok(None) => break,
                    Err(_) => {
                        self.remove_failed(region, network, now_ms);
                        stats.removed += 1;
                    }
                }
            }
        }
        Ok(stats)
    }
    pub fn flush_one<S: crate::timestamp::EpochStore>(
        &mut self,
        network: &mut crate::network::Network<S>,
        now_ms: u64,
        mut direct: impl FnMut(std::net::SocketAddr, &[u8]) -> std::io::Result<()>,
    ) -> Result<bool, crate::network::Error> {
        network.tick(now_ms)?;
        self.maintain(network, now_ms);
        network.flush_one_authorized(now_ms, |route, peer, bytes, permit| match route {
            crate::discovery::Route::Direct(endpoint) => direct(endpoint, bytes),
            crate::discovery::Route::Relay(region) => self
                .connections
                .get_mut(&region)
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::WouldBlock))?
                .accept(peer, bytes, permit, now_ms),
        })
    }
    pub fn close(&mut self) {
        self.connections.clear();
        self.failures.clear();
        self.prefer_peer = false;
        self.restarts.clear();
    }
}
struct Output {
    bytes: Vec<u8>,
    offset: usize,
    deadline: u64,
}
pub struct Connection<T: Transport> {
    tls: T,
    decoder: Decoder,
    output: VecDeque<Output>,
    last_ms: u64,
    last_input_ms: u64,
    closed: bool,
}
impl<T: Transport> Connection<T> {
    pub fn pending_output(&self) -> bool {
        !self.output.is_empty()
    }
    pub fn from_session(session: RelaySession<T::Blocking>, now_ms: u64) -> Result<Self, Error> {
        Ok(Self {
            tls: T::from_verified(session.into_transport()?)?,
            decoder: Decoder::default(),
            output: VecDeque::new(),
            last_ms: now_ms,
            last_input_ms: now_ms,
            closed: false,
        })
    }
    fn check(&mut self, now_ms: u64) -> Result<(), Error> {
        if self.closed
            || now_ms < self.last_ms
            || now_ms.saturating_sub(self.last_input_ms) >= 120000
            || self.output.iter().any(|p| p.deadline <= now_ms)
        {
            self.close();
            return Err(Error::Closed);
        }
        self.last_ms = now_ms;
        if let Err(error) = self.tls.check() {
            self.close();
            return Err(error.into());
        }
        if let Err(error) = self.decoder.maintain(now_ms) {
            self.close();
            return Err(error.into());
        }
        Ok(())
    }
    fn enqueue(&mut self, bytes: Vec<u8>, now_ms: u64) -> Result<(), Error> {
        if self.output.len() >= 2 {
            return Err(Error::Busy);
        }
        let Some(deadline) = now_ms.checked_add(5000) else {
            self.close();
            return Err(Error::Closed);
        };
        self.output.push_back(Output {
            bytes,
            offset: 0,
            deadline,
        });
        Ok(())
    }
    /// Acceptance queues one datagram, not proof of delivery. The runtime must
    /// close this connection when its queued authorization becomes invalid.
    pub fn send(&mut self, peer: PublicKey<Node>, packet: &[u8], now_ms: u64) -> Result<(), Error> {
        self.check(now_ms)?;
        let bytes = derp_frames::send_packet(peer, packet)?;
        self.enqueue(bytes, now_ms)
    }
    /// One plaintext enqueue, one socket write, and the receipt of at most
    /// one frame, which is returned as an event once it is complete. Pings queue their pong automatically.
    pub fn poll(&mut self, now_ms: u64) -> Result<Option<Event>, Error> {
        self.check(now_ms)?;
        let result = self.step(now_ms);
        if result.is_err() {
            self.close();
        }
        result
    }
    fn step(&mut self, now_ms: u64) -> Result<Option<Event>, Error> {
        if let Some(front) = self.output.front_mut()
            && front.offset < front.bytes.len()
        {
            front.offset += self.tls.queue(&front.bytes[front.offset..])?;
        }
        if self.tls.flush_pending()?
            && self
                .output
                .front()
                .is_some_and(|p| p.offset == p.bytes.len())
        {
            self.output.pop_front();
        }
        // A full packet frame takes three reads: its header and a body of up
        // to 32 + MAX_DATAGRAM bytes in two parts.
        let mut buffer = [0; 1024];
        for _ in 0..3 {
            let wanted = self.decoder.wanted().min(buffer.len());
            let Some(count) = self.tls.receive(&mut buffer[..wanted])? else {
                return Ok(None);
            };
            if count == 0 {
                self.decoder.eof()?;
                return Err(Error::Closed);
            }
            self.last_input_ms = now_ms;
            let (used, event) = self.decoder.feed(&buffer[..count], now_ms)?;
            if used != count {
                return Err(derp_frames::Error::Frame.into());
            }
            if let Some(Event::Ping(value)) = &event {
                self.enqueue(derp_frames::pong(*value), now_ms)?;
            }
            if event.is_some() {
                return Ok(event);
            }
        }
        Ok(None)
    }
    pub fn close(&mut self) {
        self.closed = true;
        self.output.clear();
        self.decoder.close();
        self.tls.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        coordinator::Clock,
        network::Network,
        protocol::identity::DeviceIdentity,
        timestamp::{EpochStore, TimestampClock},
    };
    use std::io;
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
    struct Io;
    impl io::Read for Io {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            unreachable!()
        }
    }
    impl io::Write for Io {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            unreachable!()
        }
        fn flush(&mut self) -> io::Result<()> {
            unreachable!()
        }
    }
    impl crate::derp::DeadlineIo for Io {
        fn set_deadline(&mut self, _: std::time::Instant) {}
    }
    struct Idle;
    impl Transport for Idle {
        type Blocking = Io;
        fn from_verified(_: Io) -> io::Result<Self> {
            unreachable!()
        }
        fn check(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn queue(&mut self, _: &[u8]) -> io::Result<usize> {
            unreachable!()
        }
        fn flush_pending(&mut self) -> io::Result<bool> {
            Ok(true)
        }
        fn receive(&mut self, _: &mut [u8]) -> io::Result<Option<usize>> {
            Ok(None)
        }
        fn close(&mut self) {}
    }
    fn connection(plan: crate::network::RelayPlan) -> AuthorizedConnection<Idle> {
        AuthorizedConnection {
            plan,
            permits: Vec::new(),
            connection: Connection {
                tls: Idle,
                decoder: Decoder::default(),
                output: VecDeque::new(),
                last_ms: 0,
                last_input_ms: 0,
                closed: false,
            },
        }
    }
    fn network() -> (Network<Store>, Vec<DeviceIdentity>) {
        let local = DeviceIdentity::generate().unwrap();
        let peers: Vec<_> = (0..5)
            .map(|_| DeviceIdentity::generate().unwrap())
            .collect();
        let mut regions = serde_json::Map::new();
        let mut nodes = Vec::new();
        for (index, peer) in peers.iter().enumerate() {
            let id = index + 1;
            regions.insert(id.to_string(), serde_json::json!({"RegionID":id,"Nodes":[{"Name":format!("{id}a"),"RegionID":id,"HostName":"relay.example"}]}));
            nodes.push(serde_json::json!({"ID":id+1,"Key":peer.node_key().public().to_string(),"Addresses":[format!("100.64.0.{}/32",id+1)],"HomeDERP":id,"MachineAuthorized":true}));
        }
        let map = serde_json::json!({"Node":{"ID":1,"Key":local.node_key().public().to_string(),"Addresses":["100.64.0.1/32"],"MachineAuthorized":true},"Peers":nodes,"DERPMap":{"Regions":regions},"PacketFilter":[]});
        let mut network =
            Network::new(&local, TimestampClock::open(Store, 100).unwrap(), 8, 2).unwrap();
        network
            .apply_map(
                &serde_json::to_vec(&map).unwrap(),
                Clock {
                    unix_ms: 1_735_689_600_000,
                    monotonic_ms: 0,
                },
            )
            .unwrap();
        (network, peers)
    }
    #[test]
    fn failed_home_yields_to_peer_regions_and_retries_at_deadline() {
        let (mut network, peers) = network();
        let mut pool = Pool::<Idle>::default();
        network.initiate(peers[0].node_key().public(), 0).unwrap();
        network.initiate(peers[1].node_key().public(), 0).unwrap();
        let home = pool.next_plan(&mut network, 0).unwrap().unwrap();
        assert!(home.is_home());
        pool.setup_failed(home.clone(), 0);
        // The ordinary plan for region 1 shares the home endpoint's cooldown.
        let peer = pool.next_plan(&mut network, 1).unwrap().unwrap();
        assert!(!peer.is_home());
        assert_eq!(peer.region(), 2);
        pool.setup_failed(peer, 1);
        for region in 3..=5 {
            let alternate = pool.next_plan(&mut network, 2).unwrap().unwrap();
            assert!(alternate.is_home());
            assert_eq!(alternate.region(), region);
            pool.setup_failed(alternate, 2);
        }
        assert!(pool.next_plan(&mut network, 2).unwrap().is_none());
        network
            .apply_map(
                br#"{"KeepAlive":true}"#,
                Clock {
                    unix_ms: 1_735_689_600_003,
                    monotonic_ms: 3,
                },
            )
            .unwrap();
        assert!(pool.next_plan(&mut network, 3).unwrap().is_none());
        assert!(pool.next_plan(&mut network, 29_999).unwrap().is_none());
        assert_eq!(
            pool.next_plan(&mut network, 30_000)
                .unwrap()
                .unwrap()
                .region(),
            1
        );
        pool.setup_failed(network.home_relay_plan(30_000).unwrap().unwrap(), 30_000);
        pool.close();
        assert!(
            pool.next_plan(&mut network, 30_000)
                .unwrap()
                .unwrap()
                .is_home()
        );
    }
    #[test]
    fn alternate_servers_and_regions_preserve_a_working_home() {
        let (mut network, peers) = network();
        let map = serde_json::json!({"DERPMap":{"Regions":{
            "1":{"RegionID":1,"Nodes":[
                {"Name":"1s","RegionID":1,"HostName":"stun.example","STUNOnly":true},
                {"Name":"1a","RegionID":1,"HostName":"first.example"},
                {"Name":"1b","RegionID":1,"HostName":"second.example"}]},
            "2":{"RegionID":2,"NoMeasureNoHome":true,"Nodes":[
                {"Name":"2a","RegionID":2,"HostName":"peer-first.example"},
                {"Name":"2b","RegionID":2,"HostName":"peer-second.example"}]},
            "3":{"RegionID":3,"Nodes":[{"Name":"3a","RegionID":3,"HostName":"other-home.example"}]}
        }}});
        network
            .apply_map(
                &serde_json::to_vec(&map).unwrap(),
                Clock {
                    unix_ms: 1_735_689_600_000,
                    monotonic_ms: 0,
                },
            )
            .unwrap();
        let mut pool = Pool::<Idle>::default();
        let first = pool.next_plan(&mut network, 0).unwrap().unwrap();
        assert_eq!(first.node().name, "1a");
        pool.setup_failed(first, 0);
        let second = pool.next_plan(&mut network, 1).unwrap().unwrap();
        assert_eq!(second.node().name, "1b");
        assert!(network.validate_relay_plan(&second, 1).unwrap());
        pool.setup_failed(second, 1);
        let third = pool.next_plan(&mut network, 2).unwrap().unwrap();
        assert_eq!(third.region(), 3);
        pool.insert(connection(third), &mut network, 2).unwrap();
        assert_eq!(pool.home_region(&mut network, 30_002), Some(3));
        assert!(pool.next_plan(&mut network, 30_002).unwrap().is_none());
        network
            .initiate(peers[1].node_key().public(), 30_002)
            .unwrap();
        let peer = pool.next_plan(&mut network, 30_002).unwrap().unwrap();
        assert!(!peer.is_home());
        assert_eq!(peer.node().name, "2a");
        pool.setup_failed(peer, 30_002);
        let peer = pool.next_plan(&mut network, 30_003).unwrap().unwrap();
        assert_eq!(peer.node().name, "2b");
        pool.insert(connection(peer), &mut network, 30_003).unwrap();
        assert_eq!(pool.home_region(&mut network, 30_003), Some(3));
        let replacement = network.home_relay_plan(30_003).unwrap().unwrap();
        pool.insert(connection(replacement), &mut network, 30_003)
            .unwrap();
        assert_eq!(pool.home_region(&mut network, 30_003), Some(1));
        assert!(!pool.contains_region(3));
        assert!(pool.contains_region(2));
    }
    #[test]
    fn restart_honors_delay_and_retry_window_without_blocking_other_regions() {
        let (mut network, peers) = network();
        let mut pool = Pool::<Idle>::default();
        let home = network.home_relay_plan(0).unwrap().unwrap();
        pool.insert(connection(home), &mut network, 0).unwrap();
        pool.restarting(1, &mut network, 10, 10_000, 20_000);
        assert_eq!(pool.home_region(&mut network, 10), None);
        assert!(pool.next_plan(&mut network, 10).unwrap().is_none());
        network.initiate(peers[1].node_key().public(), 11).unwrap();
        let peer = pool.next_plan(&mut network, 11).unwrap().unwrap();
        assert!(!peer.is_home());
        assert_eq!(peer.region(), 2);
        pool.insert(connection(peer), &mut network, 11).unwrap();
        assert!(pool.next_plan(&mut network, 10_009).unwrap().is_none());
        let home = pool.next_plan(&mut network, 10_010).unwrap().unwrap();
        assert_eq!(home.region(), 1);
        assert!(home.is_home());
        pool.setup_failed(home, 10_010);
        assert!(pool.next_plan(&mut network, 15_009).unwrap().is_none());
        let retry = pool.next_plan(&mut network, 15_010).unwrap().unwrap();
        assert_eq!(retry.region(), 1);
        pool.setup_failed(retry, 29_000);
        assert!(pool.next_plan(&mut network, 30_009).unwrap().is_none());
        // The other region is already connected, so it becomes the home
        // without a second connection.
        assert!(pool.next_plan(&mut network, 30_010).unwrap().is_none());
        assert!(pool.restarts.is_empty());
        assert_eq!(pool.home_region(&mut network, 34_000), Some(2));
    }
    #[test]
    fn restart_bounds_success_and_revocation() {
        let (mut network, _) = network();
        let mut pool = Pool::<Idle>::default();
        let home = network.home_relay_plan(0).unwrap().unwrap();
        pool.insert(connection(home), &mut network, 0).unwrap();
        pool.restarting(1, &mut network, 0, u32::MAX, u32::MAX);
        assert_eq!(
            (pool.restarts[0].ready_ms, pool.restarts[0].until_ms),
            (60_000, 90_000)
        );
        assert!(pool.next_plan(&mut network, 59_999).unwrap().is_none());
        let home = pool.next_plan(&mut network, 60_000).unwrap().unwrap();
        pool.insert(connection(home), &mut network, 60_000).unwrap();
        assert!(pool.restarts.is_empty());
        pool.restarting(1, &mut network, 60_000, 0, 0);
        let home = pool.next_plan(&mut network, 60_000).unwrap().unwrap();
        assert!(home.is_home());
        network.disconnect();
        assert!(pool.next_plan(&mut network, 60_000).unwrap().is_none());
        assert!(pool.restarts.is_empty());
        pool.close();
        assert!(pool.failures.is_empty());
    }
    #[test]
    fn success_clears_cooldown() {
        let (mut network, _) = network();
        let mut pool = Pool::<Idle>::default();
        let home = network.home_relay_plan(0).unwrap().unwrap();
        pool.setup_failed(home.clone(), 0);
        pool.insert(connection(home), &mut network, 1).unwrap();
        assert!(pool.failures.is_empty());
        assert_eq!(pool.home_region(&mut network, 1), Some(1));
    }
    #[test]
    fn cooldown_storage_is_bounded_across_directory_changes() {
        let (mut network, _) = network();
        let mut pool = Pool::<Idle>::default();
        for id in 1..=257 {
            let map = serde_json::json!({"DERPMap":{"Regions":{"1":{"RegionID":1,"Nodes":[{"Name":"1a","RegionID":1,"HostName":format!("relay{id}.example")}]}}}});
            network
                .apply_map(
                    &serde_json::to_vec(&map).unwrap(),
                    Clock {
                        unix_ms: 1_735_689_600_000 + id,
                        monotonic_ms: id as u64,
                    },
                )
                .unwrap();
            let plan = pool.next_plan(&mut network, id as u64).unwrap().unwrap();
            pool.setup_failed(plan.clone(), id as u64);
            pool.setup_failed(plan, id as u64);
            assert_eq!(pool.failures.len(), (id as usize).min(256));
        }
        assert!(
            !pool
                .failures
                .iter()
                .any(|(plan, _)| plan.node().host_name == "relay1.example")
        );
        assert!(pool.next_plan(&mut network, 30_257).unwrap().is_some());
        assert!(pool.failures.is_empty());
    }
    #[test]
    fn home_reserves_capacity_and_promotes_an_ordinary_same_region_connection() {
        let (mut network, peers) = network();
        let mut pool = Pool::default();
        for peer in &peers[1..] {
            let plan = network
                .relay_plan(peer.node_key().public(), 0)
                .unwrap()
                .unwrap();
            pool.insert(connection(plan), &mut network, 0).unwrap();
        }
        assert_eq!(pool.len(), 4);
        let ordinary = network
            .relay_plan(peers[0].node_key().public(), 0)
            .unwrap()
            .unwrap();
        assert!(matches!(
            pool.insert(connection(ordinary.clone()), &mut network, 0),
            Err(Error::Busy)
        ));
        let home = pool.next_plan(&mut network, 0).unwrap().unwrap();
        assert!(home.is_home());
        assert_eq!(home.region(), 1);
        pool.insert(connection(home), &mut network, 0).unwrap();
        assert_eq!(pool.len(), 4);
        assert!(!pool.contains_region(2));
        assert_eq!(pool.home_region(&mut network, 0), Some(1));
        pool.close();
        pool.insert(connection(ordinary), &mut network, 0).unwrap();
        assert_eq!(pool.home_region(&mut network, 0), None);
        assert!(pool.next_plan(&mut network, 0).unwrap().is_none());
        assert_eq!(
            pool.connections[&1].connection.output[0].bytes,
            [7, 0, 0, 0, 1, 1]
        );
        assert_eq!(pool.len(), 1);
        assert_eq!(pool.home_region(&mut network, 0), Some(1));
        network.disconnect();
        assert_eq!(pool.home_region(&mut network, 1), None);
        assert!(pool.is_empty());
    }
}

#[cfg(test)]
mod prop_tests;
