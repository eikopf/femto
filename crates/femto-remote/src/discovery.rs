//! Single-peer GATT discovery sequencing. The platform supplies I/O and deadlines.

mod characteristics;
mod reads;
pub use characteristics::{Characteristic, CharacteristicKind, HidCharacteristics};
pub use reads::{MAX_MAP_BYTES, ReadKind, ReadRequest, ReadResponse, ReadSlot};

pub const MAX_PEER_BYTES: usize = 64;
pub const MAX_CHARACTERISTICS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressType {
    Public,
    Random,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer {
    address: [u8; 6],
    address_type: AddressType,
}

impl Peer {
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_PEER_BYTES {
            return Err("peer configuration limit");
        }
        let text = std::str::from_utf8(bytes).map_err(|_| "peer configuration encoding")?;
        let mut words = text.split_ascii_whitespace();
        let address_type = match words.next() {
            Some("public") => AddressType::Public,
            Some("random") => AddressType::Random,
            _ => return Err("peer address type"),
        };
        let text = words.next().ok_or("missing peer address")?;
        if words.next().is_some() || text.len() != 17 {
            return Err("peer address format");
        }
        let mut address = [0; 6];
        let mut parts = text.split(':');
        for byte in &mut address {
            let part = parts.next().ok_or("peer address format")?;
            if part.len() != 2 || !part.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err("peer address format");
            }
            *byte = u8::from_str_radix(part, 16).map_err(|_| "peer address format")?;
        }
        if parts.next().is_some() || address == [0; 6] || address == [255; 6] {
            return Err("invalid peer address");
        }
        Ok(Self {
            address,
            address_type,
        })
    }

    pub fn address(self) -> [u8; 6] {
        self.address
    }

    pub fn address_type(self) -> AddressType {
        self.address_type
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Connection {
    pub interface: u8,
    pub id: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceRange {
    pub start: u16,
    pub end: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Open {
        interface: u8,
        peer: Peer,
    },
    SearchHid(Connection),
    Inspect {
        connection: Connection,
        service: ServiceRange,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Registering,
    Connecting,
    Searching,
    Inspecting,
    Reading,
    Done,
    Failed,
}

pub struct Discovery {
    peer: Peer,
    phase: Phase,
    interface: Option<u8>,
    connection: Option<Connection>,
    service: Option<ServiceRange>,
    command: Option<Command>,
    error: Option<&'static str>,
    reads: ReadSlot,
}

impl Discovery {
    pub fn new(peer: Peer) -> Self {
        Self {
            peer,
            phase: Phase::Registering,
            interface: None,
            connection: None,
            service: None,
            command: None,
            error: None,
            reads: ReadSlot::default(),
        }
    }

    pub fn interface(&self) -> Option<u8> {
        self.interface
    }
    pub fn connection(&self) -> Option<Connection> {
        self.connection
    }
    pub fn error(&self) -> Option<&'static str> {
        self.error.or(self.reads.error())
    }
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }
    pub fn is_discovered(&self) -> bool {
        self.phase == Phase::Reading && self.error().is_none()
    }
    pub fn begin_read(&mut self, request: ReadRequest) -> Result<(), &'static str> {
        if !self.is_discovered() || self.connection != Some(request.connection) {
            return Err("read outside discovered connection");
        }
        self.reads.begin(request)
    }
    pub fn read_completed(&mut self, request: ReadRequest, success: bool, bytes: &[u8]) {
        if self.is_discovered() {
            self.reads.complete(request, success, bytes);
        }
    }
    pub fn take_read(&mut self) -> Result<Option<ReadResponse>, &'static str> {
        self.reads.take()
    }
    pub fn finish(&mut self, connection: Connection) {
        if self.is_discovered() && self.connection == Some(connection) && self.reads.is_idle() {
            self.phase = Phase::Done;
        }
    }
    pub fn take_command(&mut self) -> Option<Command> {
        self.command.take()
    }

    /// Failure clears pending work but retains handles for platform shutdown.
    pub fn abort(&mut self, error: &'static str) {
        if self.phase != Phase::Failed {
            self.error = self.error().or(Some(error));
            self.phase = Phase::Failed;
            self.command = None;
            self.reads.cancel();
        }
    }

    pub fn registered(&mut self, interface: u8, success: bool) {
        if self.phase != Phase::Registering {
            return;
        }
        if !success || interface == u8::MAX {
            self.abort("GATT registration failed");
            return;
        }
        self.interface = Some(interface);
        self.phase = Phase::Connecting;
        self.command = Some(Command::Open {
            interface,
            peer: self.peer,
        });
    }

    pub fn opened(&mut self, connection: Connection, address: [u8; 6], success: bool) {
        if self.phase != Phase::Connecting
            || self.command.is_some()
            || self.interface != Some(connection.interface)
        {
            return;
        }
        if success {
            self.connection = Some(connection);
        }
        if !success || address != self.peer.address {
            self.abort("GATT peer connection failed or mismatched");
            return;
        }
        self.phase = Phase::Searching;
        self.command = Some(Command::SearchHid(connection));
    }

    pub fn hid_service(&mut self, connection: Connection, service: ServiceRange, primary: bool) {
        if self.phase != Phase::Searching
            || self.command.is_some()
            || self.connection != Some(connection)
        {
            return;
        }
        if !primary || service.start == 0 || service.start >= service.end || self.service.is_some()
        {
            self.abort("ambiguous or invalid HID service");
            return;
        }
        self.service = Some(service);
    }

    pub fn searched(&mut self, connection: Connection, success: bool) {
        if self.phase != Phase::Searching
            || self.command.is_some()
            || self.connection != Some(connection)
        {
            return;
        }
        let Some(service) = self.service.filter(|_| success) else {
            self.abort("HID service discovery failed or missing");
            return;
        };
        self.phase = Phase::Inspecting;
        self.command = Some(Command::Inspect {
            connection,
            service,
        });
    }

    pub fn inspected(&mut self, connection: Connection) {
        if self.phase == Phase::Inspecting
            && self.command.is_none()
            && self.connection == Some(connection)
        {
            self.phase = Phase::Reading;
        }
    }

    pub fn disconnected(&mut self, connection: Connection) {
        if self.connection == Some(connection) {
            self.abort("GATT peer disconnected");
        }
    }

    pub fn service_changed(&mut self, address: [u8; 6]) {
        if address == self.peer.address {
            self.abort("peer services changed during discovery");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const CONNECTION: Connection = Connection {
        interface: 4,
        id: 12,
    };
    const SERVICE: ServiceRange = ServiceRange { start: 10, end: 50 };
    fn peer() -> Peer {
        Peer::parse(b"public 12:34:56:78:9a:BC\n").unwrap()
    }
    fn searching() -> Discovery {
        let mut state = Discovery::new(peer());
        state.registered(4, true);
        assert_eq!(
            state.take_command(),
            Some(Command::Open {
                interface: 4,
                peer: peer()
            })
        );
        state.opened(CONNECTION, peer().address(), true);
        assert_eq!(state.take_command(), Some(Command::SearchHid(CONNECTION)));
        state
    }

    fn reading() -> Discovery {
        let mut state = searching();
        state.hid_service(CONNECTION, SERVICE, true);
        state.searched(CONNECTION, true);
        state.take_command();
        state.inspected(CONNECTION);
        state
    }

    #[test]
    fn assembled_map_and_reference_produce_a_valid_input_route() {
        use crate::{
            RemoteInput,
            gatt::{InputReport, ReportRoutes},
        };
        let mut state = reading();
        let map = ReadRequest {
            connection: CONNECTION,
            handle: 11,
            kind: ReadKind::ReportMap,
        };
        state.begin_read(map).unwrap();
        state.read_completed(
            map,
            true,
            &[
                0x05, 7, 0x19, 0, 0x29, 255, 0x15, 0, 0x26, 255, 0, 0x75, 8, 0x95, 6, 0x81, 0,
            ],
        );
        state.finish(CONNECTION);
        assert!(!state.is_done());
        let response = state.take_read().unwrap().unwrap();
        let mut input = RemoteInput::new(response.bytes()).unwrap();
        let reference = ReadRequest {
            handle: 21,
            kind: ReadKind::ReportReference,
            ..map
        };
        state.begin_read(reference).unwrap();
        state.read_completed(reference, true, &[0, 1]);
        let response = state.take_read().unwrap().unwrap();
        let routes = ReportRoutes::new(
            &input,
            &[InputReport {
                value_handle: 20,
                cccd_handle: 22,
                reference: response.bytes().try_into().unwrap(),
            }],
        )
        .unwrap();
        state.finish(CONNECTION);
        assert!(state.is_done());
        assert_eq!(
            input
                .accept(&routes.reference(20).unwrap(), &[0x4e, 0, 0, 0, 0, 0])
                .unwrap()
                .as_slice(),
            &[0x7004e]
        );
    }

    #[test]
    fn disconnect_service_change_and_timeout_discard_completed_reads() {
        let request = ReadRequest {
            connection: CONNECTION,
            handle: 11,
            kind: ReadKind::ReportMap,
        };
        for reason in 0..3 {
            let mut state = reading();
            state.begin_read(request).unwrap();
            state.read_completed(request, true, &[1, 2]);
            match reason {
                0 => state.disconnected(CONNECTION),
                1 => state.service_changed(peer().address()),
                _ => state.abort("timeout"),
            }
            assert!(state.take_read().is_err());
            assert!(state.begin_read(request).is_err());
            state.read_completed(request, true, &[1, 2]);
            state.finish(CONNECTION);
            assert!(!state.is_done());
            assert_eq!(state.connection(), Some(CONNECTION));
        }
    }

    #[test]
    fn completed_discovery_remains_revocable_for_an_active_connection() {
        let mut state = reading();
        state.finish(CONNECTION);
        assert!(state.is_done());
        state.disconnected(CONNECTION);
        assert!(!state.is_done());
        assert!(state.error().is_some());
        assert_eq!(state.connection(), Some(CONNECTION));
        state.finish(CONNECTION);
        assert!(!state.is_done());
    }

    #[test]
    fn configured_peer_is_strict_and_bounded() {
        assert_eq!(peer().address(), [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
        assert_eq!(
            Peer::parse(b"random C0:12:34:56:78:9A")
                .unwrap()
                .address_type(),
            AddressType::Random
        );
        for bad in [
            "",
            "public",
            "private 12:34:56:78:9A:BC",
            "public 1:34:56:78:9A:BC",
            "public +1:34:56:78:9A:BC",
            "public 12:34:56:78:9G:BC",
            "public 12:34:56:78:9A:BC extra",
            "public 00:00:00:00:00:00",
            "random FF:FF:FF:FF:FF:FF",
        ] {
            assert!(Peer::parse(bad.as_bytes()).is_err(), "{bad}");
        }
        assert!(Peer::parse(&[b' '; 65]).is_err());
        assert!(Peer::parse(&[255]).is_err());
    }

    #[test]
    fn successful_discovery_issues_each_operation_once() {
        let mut state = searching();
        assert_eq!(state.take_command(), None);
        state.hid_service(CONNECTION, SERVICE, true);
        state.searched(CONNECTION, true);
        assert!(!state.is_done());
        state.inspected(CONNECTION);
        assert!(!state.is_done());
        assert_eq!(
            state.take_command(),
            Some(Command::Inspect {
                connection: CONNECTION,
                service: SERVICE
            })
        );
        state.inspected(CONNECTION);
        assert!(state.is_discovered());
        state.finish(CONNECTION);
        assert!(state.is_done());
        state.searched(CONNECTION, true);
        assert_eq!(state.take_command(), None);
    }

    #[test]
    fn wrong_connections_and_out_of_order_events_cannot_advance_discovery() {
        let mut state = Discovery::new(peer());
        state.registered(4, true);
        state.opened(CONNECTION, peer().address(), true);
        assert!(matches!(state.take_command(), Some(Command::Open { .. })));
        state.opened(
            Connection {
                interface: 5,
                ..CONNECTION
            },
            peer().address(),
            true,
        );
        assert_eq!(state.connection(), None);
        state.opened(CONNECTION, peer().address(), true);
        state.hid_service(CONNECTION, SERVICE, true);
        assert_eq!(state.take_command(), Some(Command::SearchHid(CONNECTION)));
        let other = Connection {
            id: 13,
            ..CONNECTION
        };
        state.hid_service(other, SERVICE, true);
        state.searched(other, true);
        state.disconnected(other);
        assert_eq!(state.error(), None);
        state.searched(CONNECTION, true);
        assert!(state.error().is_some());
    }

    #[test]
    fn failures_clear_queued_work_and_keep_cleanup_handles() {
        let mut state = searching();
        state.hid_service(CONNECTION, SERVICE, true);
        state.searched(CONNECTION, true);
        state.disconnected(CONNECTION);
        assert_eq!(state.take_command(), None);
        assert_eq!(state.connection(), Some(CONNECTION));
        state.inspected(CONNECTION);
        assert!(!state.is_done());
        let error = state.error();
        state.abort("timeout");
        assert_eq!(state.error(), error);
        for (service, primary) in [
            (ServiceRange { start: 0, end: 50 }, true),
            (ServiceRange { start: 50, end: 10 }, true),
            (SERVICE, false),
        ] {
            let mut state = searching();
            state.hid_service(CONNECTION, service, primary);
            assert!(state.error().is_some());
        }
        let mut state = searching();
        state.hid_service(CONNECTION, SERVICE, true);
        state.hid_service(CONNECTION, SERVICE, true);
        assert!(state.error().is_some());
    }

    #[test]
    fn registration_open_and_service_change_fail_closed() {
        let mut state = Discovery::new(peer());
        state.registered(4, false);
        state.registered(4, true);
        assert_eq!(state.take_command(), None);
        for (address, success) in [([3; 6], true), (peer().address(), false)] {
            let mut state = Discovery::new(peer());
            state.registered(4, true);
            state.take_command();
            state.opened(CONNECTION, address, success);
            assert!(state.error().is_some());
            assert_eq!(state.take_command(), None);
        }
        let mut state = searching();
        state.service_changed([3; 6]);
        assert_eq!(state.error(), None);
        state.service_changed(peer().address());
        assert!(state.error().is_some());
    }
}
