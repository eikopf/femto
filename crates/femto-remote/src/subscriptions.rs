//! Serial notification registration and acknowledged CCCD writes.

use crate::{
    discovery::Connection,
    gatt::{InputReport, ReportRoutes},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Register(u16),
    Enable(u16),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Registering,
    Enabling,
    Ready,
    Failed,
}

pub struct Subscriptions {
    connection: Connection,
    reports: [InputReport; 8],
    len: usize,
    index: usize,
    phase: Phase,
    command: Option<Command>,
    error: Option<&'static str>,
}

impl Subscriptions {
    pub fn new(connection: Connection, routes: &ReportRoutes) -> Self {
        let mut reports = [InputReport::default(); 8];
        let len = routes.reports().len();
        reports[..len].copy_from_slice(routes.reports());
        Self {
            connection,
            reports,
            len,
            index: 0,
            phase: Phase::Registering,
            command: Some(Command::Register(reports[0].value_handle)),
            error: None,
        }
    }
    pub fn is_ready(&self) -> bool {
        self.phase == Phase::Ready
    }
    pub fn error(&self) -> Option<&'static str> {
        self.error
    }
    pub fn take_command(&mut self) -> Option<Command> {
        self.command.take()
    }
    pub fn cancel(&mut self) {
        self.fail("subscriptions cancelled");
    }
    fn fail(&mut self, error: &'static str) {
        self.error.get_or_insert(error);
        self.command = None;
        self.phase = Phase::Failed;
    }
    pub fn registered(&mut self, interface: u8, handle: u16, success: bool) {
        if interface != self.connection.interface || self.phase == Phase::Failed {
            return;
        }
        if self.phase != Phase::Registering
            || self.command.is_some()
            || handle != self.reports[self.index].value_handle
            || !success
        {
            self.fail("notification registration failed or unexpected");
            return;
        }
        self.phase = Phase::Enabling;
        self.command = Some(Command::Enable(self.reports[self.index].cccd_handle));
    }
    pub fn configured(&mut self, connection: Connection, handle: u16, success: bool) {
        if connection != self.connection || self.phase == Phase::Failed {
            return;
        }
        if self.phase != Phase::Enabling
            || self.command.is_some()
            || handle != self.reports[self.index].cccd_handle
            || !success
        {
            self.fail("notification configuration failed or unexpected");
            return;
        }
        self.index += 1;
        if self.index == self.len {
            self.phase = Phase::Ready;
        } else {
            self.phase = Phase::Registering;
            self.command = Some(Command::Register(self.reports[self.index].value_handle));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RemoteInput;
    const CONNECTION: Connection = Connection {
        interface: 1,
        id: 2,
    };
    fn subscriptions() -> Subscriptions {
        subscriptions_count(2)
    }
    fn subscriptions_count(count: u8) -> Subscriptions {
        let array = &[
            0x05, 7, 0x19, 0, 0x29, 255, 0x15, 0, 0x26, 255, 0, 0x75, 8, 0x95, 6, 0x81, 0,
        ];
        let mut map = Vec::new();
        let mut reports = Vec::new();
        for id in 1..=count {
            map.extend_from_slice(&[0x85, id]);
            map.extend_from_slice(array);
            let handle = u16::from(id) * 10;
            reports.push(InputReport {
                value_handle: handle,
                cccd_handle: handle + 1,
                reference: [id, 1],
            });
        }
        let input = RemoteInput::new(&map).unwrap();
        let routes = ReportRoutes::new(&input, &reports).unwrap();
        Subscriptions::new(CONNECTION, &routes)
    }
    #[test]
    fn eight_routes_finish_without_indexing_beyond_the_last_report() {
        let mut state = subscriptions_count(8);
        for handle in (10..=80).step_by(10) {
            assert_eq!(state.take_command(), Some(Command::Register(handle)));
            state.registered(1, handle, true);
            assert_eq!(state.take_command(), Some(Command::Enable(handle + 1)));
            state.configured(CONNECTION, handle + 1, true);
        }
        assert!(state.is_ready());
        state.registered(1, 80, true);
        assert!(state.error().is_some());
        state.configured(CONNECTION, 81, true);
        assert!(!state.is_ready());
    }
    #[test]
    fn all_registration_and_write_acknowledgements_are_required() {
        let mut state = subscriptions();
        for handle in [10, 20] {
            assert!(!state.is_ready());
            assert_eq!(state.take_command(), Some(Command::Register(handle)));
            state.registered(9, handle, true);
            assert_eq!(state.take_command(), None);
            state.registered(1, handle, true);
            assert_eq!(state.take_command(), Some(Command::Enable(handle + 1)));
            state.configured(
                Connection {
                    id: 3,
                    ..CONNECTION
                },
                handle + 1,
                true,
            );
            assert!(!state.is_ready());
            state.configured(CONNECTION, handle + 1, true);
        }
        assert!(state.is_ready());
        assert_eq!(state.take_command(), None);
        state.configured(CONNECTION, 21, true);
        assert!(!state.is_ready());
        assert!(state.error().is_some());
    }
    #[test]
    fn failures_cancellation_and_early_acknowledgements_never_enable_input() {
        for failure in 0..5 {
            let mut state = subscriptions();
            if failure == 0 {
                state.registered(1, 10, true);
            } else {
                state.take_command();
                match failure {
                    1 => state.registered(1, 20, true),
                    2 => state.registered(1, 10, false),
                    3 => {
                        state.registered(1, 10, true);
                        state.take_command();
                        state.configured(CONNECTION, 11, false);
                    }
                    _ => state.cancel(),
                }
            }
            assert!(state.error().is_some());
            assert!(!state.is_ready());
            assert_eq!(state.take_command(), None);
            state.registered(1, 10, true);
            assert!(!state.is_ready());
        }
    }
}
