//! Serialized remote input dispatch through the shared reader controller.

mod settings;
pub use settings::{RemoteAction, RemoteBinding, SavedBindings};

use crate::{
    Error, Result,
    controller::{Effect, Reader},
    ui::ReaderUi,
};
pub use femto_remote::{
    Bindings, PageTurn,
    gatt::InputReport,
    inbox::{SessionToken, Submit},
};
use femto_remote::{
    RemoteInput,
    gatt::ReportRoutes,
    inbox::{Event, Inbox, ResetReason},
};

#[derive(Default)]
pub struct RemoteControl {
    inbox: Inbox,
    session: Option<(SessionToken, RemoteInput)>,
    routes: Option<ReportRoutes>,
}

impl RemoteControl {
    /// Validates a new map before replacing the active connection's input state.
    pub fn connect(&mut self, descriptor: &[u8]) -> Result<SessionToken> {
        let input = RemoteInput::new(descriptor).map_err(remote_error)?;
        self.begin(input, None)
    }

    /// Installs a report map and its validated GATT input characteristics together.
    pub fn connect_gatt(
        &mut self,
        descriptor: &[u8],
        reports: &[InputReport],
    ) -> Result<SessionToken> {
        let input = RemoteInput::new(descriptor).map_err(remote_error)?;
        let routes = ReportRoutes::new(&input, reports).map_err(remote_error)?;
        self.begin(input, Some(routes))
    }

    fn begin(&mut self, input: RemoteInput, routes: Option<ReportRoutes>) -> Result<SessionToken> {
        let token = match self.inbox.begin() {
            Ok(token) => token,
            Err(error) => {
                self.session = None;
                self.routes = None;
                return Err(remote_error(error));
            }
        };
        self.session = Some((token, input));
        self.routes = routes;
        Ok(token)
    }

    /// Unknown handles invalidate current input; stale callbacks have no effect.
    pub fn submit_gatt(&mut self, token: SessionToken, handle: u16, payload: &[u8]) -> Submit {
        if !self.inbox.is_current(token) {
            return Submit::Stale;
        }
        let Some(reference) = self
            .routes
            .as_ref()
            .and_then(|routes| routes.reference(handle))
        else {
            self.inbox.disconnect(token);
            return Submit::Invalidated;
        };
        self.inbox.submit(token, reference, payload)
    }

    pub fn is_active(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|(token, _)| self.inbox.is_current(*token))
    }

    pub fn submit(&mut self, token: SessionToken, reference: [u8; 2], payload: &[u8]) -> Submit {
        self.inbox.submit(token, reference, payload)
    }

    pub fn disconnect(&mut self, token: SessionToken) {
        self.inbox.disconnect(token);
    }

    /// Processes one event. Call under the same serialization as submit/disconnect.
    pub fn poll(&mut self, reader: &mut Reader, ui: &mut ReaderUi) -> Result<Effect> {
        match self.inbox.poll() {
            None => Ok(Effect::Unchanged),
            Some(Event::Reset(ResetReason::SessionChanged)) => {
                if let Some((_, input)) = self.session.as_mut() {
                    input.reset();
                }
                Ok(Effect::Unchanged)
            }
            Some(Event::Reset(_)) => {
                self.session = None;
                self.routes = None;
                Ok(Effect::Unchanged)
            }
            Some(Event::Input(notification)) => {
                let Some((token, input)) = self.session.as_mut() else {
                    return Ok(Effect::Unchanged);
                };
                if *token != notification.token || !self.inbox.is_current(*token) {
                    return Ok(Effect::Unchanged);
                }
                let presses = match input.accept(&notification.reference, notification.payload()) {
                    Ok(presses) => presses,
                    Err(error) => {
                        self.inbox.disconnect(*token);
                        self.session = None;
                        self.routes = None;
                        return Err(remote_error(error));
                    }
                };
                let bindings = match &reader.state.remote_bindings {
                    Some(saved) => saved.bindings()?,
                    None => Bindings::default(),
                };
                let action = bindings.resolve(&presses);
                if ui.is_open() {
                    return Ok(Effect::Unchanged);
                }
                match action {
                    Some(PageTurn::Next) => ui.turn(reader, true),
                    Some(PageTurn::Previous) => ui.turn(reader, false),
                    None => Ok(Effect::Unchanged),
                }
            }
        }
    }
}

fn remote_error(error: &'static str) -> Error {
    Error::Document(format!("remote input: {error}"))
}
