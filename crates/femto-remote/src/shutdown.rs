//! Tracks one GATT application's resources through asynchronous shutdown.
//! The caller serializes events and command dispatch and supplies a deadline.

use crate::discovery::Connection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Close(Connection),
    Unregister(u8),
}

pub struct Shutdown {
    registering: bool,
    interface: Option<u8>,
    opening: bool,
    connection: Option<Connection>,
    stopping: bool,
    closing: bool,
    unregistering: bool,
}

impl Default for Shutdown {
    fn default() -> Self {
        Self {
            registering: true,
            interface: None,
            opening: false,
            connection: None,
            stopping: false,
            closing: false,
            unregistering: false,
        }
    }
}

impl Shutdown {
    pub fn registered(&mut self, interface: u8, success: bool) {
        if self.registering {
            self.registering = false;
            if success && interface != u8::MAX {
                self.interface = Some(interface);
            }
        }
    }

    /// Called under the event lock before dispatching an open operation.
    pub fn opening(&mut self, interface: u8) -> bool {
        if self.stopping
            || self.opening
            || self.connection.is_some()
            || self.interface != Some(interface)
        {
            return false;
        }
        self.opening = true;
        true
    }

    pub fn opened(&mut self, connection: Connection, success: bool) {
        if self.opening && self.interface == Some(connection.interface) {
            self.opening = false;
            if success {
                self.connection = Some(connection);
            }
        }
    }

    /// A synchronously rejected open cannot produce a completion callback.
    pub fn open_rejected(&mut self) {
        self.opening = false;
    }

    pub fn closed(&mut self, connection: Connection, success: bool) {
        if success && self.connection == Some(connection) {
            self.connection = None;
            self.closing = false;
        }
    }

    pub fn unregistered(&mut self, interface: u8) {
        if self.unregistering && self.interface == Some(interface) {
            self.interface = None;
            self.unregistering = false;
        }
    }

    pub fn stop(&mut self) {
        self.stopping = true;
    }
    pub fn is_stopping(&self) -> bool {
        self.stopping
    }
    pub fn is_done(&self) -> bool {
        self.stopping && !self.registering && !self.opening && self.interface.is_none()
    }
    pub fn interface(&self) -> Option<u8> {
        self.interface
    }
    pub fn connection(&self) -> Option<Connection> {
        self.connection
    }

    /// Each operation is emitted once. Failure requires platform fallback,
    /// and missing acknowledgements require the caller's shutdown deadline.
    pub fn take_command(&mut self) -> Option<Command> {
        if !self.stopping || self.registering || self.opening {
            return None;
        }
        if let Some(connection) = self.connection {
            if self.closing {
                return None;
            }
            self.closing = true;
            return Some(Command::Close(connection));
        }
        if let Some(interface) = self.interface {
            if self.unregistering {
                return None;
            }
            self.unregistering = true;
            return Some(Command::Unregister(interface));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const CONNECTION: Connection = Connection {
        interface: 4,
        id: 12,
    };

    #[test]
    fn cancellation_waits_for_registration_and_matching_unregister_ack() {
        let mut s = Shutdown::default();
        s.stop();
        assert!(!s.is_done());
        assert_eq!(s.take_command(), None);
        s.registered(4, true);
        assert!(!s.opening(4));
        assert_eq!(s.take_command(), Some(Command::Unregister(4)));
        assert_eq!(s.take_command(), None);
        s.unregistered(5);
        assert!(!s.is_done());
        s.unregistered(4);
        assert!(s.is_done());
        s.registered(9, true);
        assert_eq!(s.take_command(), None);
    }

    #[test]
    fn late_open_is_closed_before_unregistering() {
        let mut s = Shutdown::default();
        s.registered(4, true);
        assert!(s.opening(4));
        s.stop();
        assert_eq!(s.take_command(), None);
        s.opened(
            Connection {
                interface: 5,
                id: 12,
            },
            true,
        );
        assert_eq!(s.take_command(), None);
        s.opened(CONNECTION, true);
        assert_eq!(s.take_command(), Some(Command::Close(CONNECTION)));
        assert_eq!(s.take_command(), None);
        s.closed(CONNECTION, false);
        s.closed(
            Connection {
                interface: 4,
                id: 13,
            },
            true,
        );
        assert_eq!(s.take_command(), None);
        s.closed(CONNECTION, true);
        assert_eq!(s.take_command(), Some(Command::Unregister(4)));
        s.unregistered(4);
        assert!(s.is_done());
    }

    #[test]
    fn failed_registration_and_open_do_not_wait_for_nonexistent_resources() {
        for interface in [4, u8::MAX] {
            let mut s = Shutdown::default();
            s.registered(interface, false);
            s.stop();
            assert!(s.is_done());
            assert_eq!(s.take_command(), None);
        }
        for rejected in [false, true] {
            let mut s = Shutdown::default();
            s.registered(4, true);
            assert!(s.opening(4));
            s.stop();
            if rejected {
                s.open_rejected();
            } else {
                s.opened(CONNECTION, false);
            }
            assert_eq!(s.take_command(), Some(Command::Unregister(4)));
        }
    }

    #[test]
    fn spontaneous_disconnect_during_operation_or_close_is_idempotent() {
        for stopping in [false, true] {
            let mut s = Shutdown::default();
            s.registered(4, true);
            assert!(s.opening(4));
            s.opened(CONNECTION, true);
            if stopping {
                s.stop();
                assert_eq!(s.take_command(), Some(Command::Close(CONNECTION)));
            }
            s.closed(CONNECTION, true);
            s.closed(CONNECTION, true);
            s.stop();
            assert_eq!(s.take_command(), Some(Command::Unregister(4)));
            assert_eq!(s.take_command(), None);
            assert!(!s.is_done());
        }
    }
}
