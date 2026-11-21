//! Network settings edited on the device through the reader's settings panel.
use crate::panel::Screen;
use crate::services::{Form, Kind};
use femto_reader::ui::{DeviceEvent, DeviceRow, DeviceRowKind, ReaderUi};

enum Page {
    Root,
    Wifi,
    Network(String),
    /// One service's settings, loaded when the page opens.
    Service(Box<dyn Form>),
}

/// What activating or filling a row does on the current page.
enum Row {
    None,
    OpenWifi,
    ToggleWifi,
    Scan,
    OpenNetwork(String),
    Connect,
    Password,
    Forget,
    OpenService(usize),
    /// A field of the open service.
    Field(usize),
    Restart,
}

pub struct Setup {
    page: Page,
    rows: Vec<Row>,
    open: bool,
    /// Service settings changed; services read them at startup.
    restart_needed: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            page: Page::Root,
            rows: Vec::new(),
            open: false,
            restart_needed: false,
        }
    }
}

fn row(label: impl Into<String>, kind: DeviceRowKind) -> DeviceRow {
    DeviceRow {
        label: label.into(),
        kind,
    }
}

impl Setup {
    pub fn event(
        &mut self,
        event: DeviceEvent,
        screen: &mut Screen,
        ui: &mut ReaderUi,
    ) -> Result<(), String> {
        let result = self.handle(event, screen, ui);
        if self.open {
            self.show(screen, ui)?;
        }
        result
    }

    fn handle(
        &mut self,
        event: DeviceEvent,
        screen: &mut Screen,
        ui: &mut ReaderUi,
    ) -> Result<(), String> {
        let wifi = screen.wifi().ok_or("Wi-Fi hardware unavailable")?;
        match event {
            DeviceEvent::Open => {
                self.page = Page::Root;
                self.open = true;
            }
            DeviceEvent::Back => match self.page {
                Page::Root => {
                    self.open = false;
                    ui.close_device();
                }
                Page::Wifi | Page::Service(_) => self.page = Page::Root,
                Page::Network(_) => self.page = Page::Wifi,
            },
            DeviceEvent::Activate(index) => match self.rows.get(index).unwrap_or(&Row::None) {
                Row::OpenWifi => {
                    self.page = Page::Wifi;
                    if wifi.enabled() && wifi.visible().is_empty() {
                        wifi.scan()?;
                    }
                }
                Row::ToggleWifi => wifi.set_enabled(!wifi.enabled())?,
                Row::Scan => wifi.scan()?,
                Row::OpenNetwork(ssid) => self.page = Page::Network(ssid.clone()),
                Row::Connect => {
                    if let Page::Network(ssid) = &self.page {
                        // An open network is remembered with an empty password.
                        let password = (!wifi.is_saved(ssid)).then_some("");
                        wifi.join(ssid, password)?;
                        self.page = Page::Wifi;
                    }
                }
                Row::Forget => {
                    if let Page::Network(ssid) = &self.page {
                        wifi.forget(ssid)?;
                        self.page = Page::Wifi;
                    }
                }
                Row::OpenService(index) => {
                    let [catalog, kosync, tailnet] = crate::services::forms();
                    self.page = Page::Service(match index {
                        0 => catalog,
                        1 => kosync,
                        _ => tailnet,
                    });
                }
                Row::Field(field) => {
                    if let Page::Service(form) = &mut self.page {
                        form.toggle(*field);
                        form.store()?;
                        self.restart_needed = true;
                    }
                }
                Row::Restart => unsafe { esp_idf_svc::sys::esp_restart() },
                Row::None | Row::Password => {}
            },
            DeviceEvent::Text(index, text) => match (self.rows.get(index), &mut self.page) {
                (Some(Row::Password), Page::Network(ssid)) => {
                    let text = zeroize::Zeroizing::new(text);
                    wifi.join(ssid, Some(&text))?;
                    self.page = Page::Wifi;
                }
                (Some(Row::Field(field)), Page::Service(form)) => {
                    form.set_text(*field, text);
                    form.store()?;
                    self.restart_needed = true;
                }
                _ => {}
            },
        }
        Ok(())
    }

    /// Refreshes the panel when scan results or the connection changed.
    /// Returns whether the view needs redrawing.
    pub fn poll(&mut self, screen: &mut Screen, ui: &mut ReaderUi) -> bool {
        let changed = screen.wifi().is_some_and(|wifi| wifi.take_changed());
        // The keyboard keeps the panel it was opened from.
        if !changed || !self.open || !ui.is_device_open() || ui.is_editing() {
            return false;
        }
        match self.show(screen, ui) {
            Ok(()) => true,
            Err(error) => {
                ui.show_error(error);
                true
            }
        }
    }

    fn show(&mut self, screen: &mut Screen, ui: &mut ReaderUi) -> Result<(), String> {
        let address = screen.wifi().and_then(|wifi| wifi.address());
        let wifi = screen.wifi().ok_or("Wi-Fi hardware unavailable")?;
        let status = match (wifi.enabled(), wifi.connected(), wifi.target()) {
            (false, _, _) => "Off".to_string(),
            (true, Some(ssid), _) => match address {
                Some(address) => format!("Connected to {ssid} ({address})"),
                None => format!("Connected to {ssid}"),
            },
            (true, None, Some(ssid)) => format!("Connecting to {ssid}"),
            (true, None, None) => "Not connected".to_string(),
        };
        let mut rows = Vec::new();
        let mut actions = Vec::new();
        let mut push = |label: String, kind: DeviceRowKind, action: Row| {
            rows.push(row(label, kind));
            actions.push(action);
        };
        let title = match &self.page {
            Page::Root => {
                push(
                    format!("Wi-Fi: {status}"),
                    DeviceRowKind::Submenu,
                    Row::OpenWifi,
                );
                for (index, form) in crate::services::forms().iter().enumerate() {
                    let state = if form.check().is_ok() {
                        "set"
                    } else {
                        "not set"
                    };
                    push(
                        format!("{}: {state}", form.title()),
                        DeviceRowKind::Submenu,
                        Row::OpenService(index),
                    );
                }
                if self.restart_needed {
                    push(
                        "Restart to apply changes".into(),
                        DeviceRowKind::Action,
                        Row::Restart,
                    );
                }
                "Network and services".to_string()
            }
            Page::Service(form) => {
                for (index, field) in form.fields().into_iter().enumerate() {
                    let kind = match field.kind {
                        Kind::Text => DeviceRowKind::Text {
                            value: field.value,
                            secret: false,
                            limit: field.limit,
                        },
                        Kind::Secret => DeviceRowKind::Text {
                            value: field.value,
                            secret: true,
                            limit: field.limit,
                        },
                        Kind::Toggle(on) => DeviceRowKind::Toggle(on),
                    };
                    push(field.label.into(), kind, Row::Field(index));
                }
                let state = match form.check() {
                    Ok(()) if self.restart_needed => "Ready after restart".to_string(),
                    Ok(()) => "Ready".to_string(),
                    Err(error) => format!("Incomplete: {error}"),
                };
                push(state, DeviceRowKind::Info, Row::None);
                form.title().to_string()
            }
            Page::Wifi => {
                push(
                    "Wi-Fi".into(),
                    DeviceRowKind::Toggle(wifi.enabled()),
                    Row::ToggleWifi,
                );
                push(status, DeviceRowKind::Info, Row::None);
                if wifi.enabled() {
                    for network in wifi.visible() {
                        let note = match (wifi.is_saved(&network.ssid), network.secured) {
                            (true, _) => " (saved)",
                            (false, false) => " (open)",
                            (false, true) => "",
                        };
                        push(
                            format!("{}{note}", network.ssid),
                            DeviceRowKind::Submenu,
                            Row::OpenNetwork(network.ssid.clone()),
                        );
                    }
                    if wifi.is_scanning() {
                        push("Scanning".into(), DeviceRowKind::Info, Row::None);
                    } else {
                        push("Scan again".into(), DeviceRowKind::Action, Row::Scan);
                    }
                }
                "Wi-Fi".to_string()
            }
            Page::Network(ssid) => {
                let saved = wifi.is_saved(ssid);
                let secured = wifi
                    .visible()
                    .iter()
                    .find(|network| &network.ssid == ssid)
                    .is_none_or(|network| network.secured);
                if saved || !secured {
                    push("Connect".into(), DeviceRowKind::Action, Row::Connect);
                }
                if secured {
                    push(
                        "Password".into(),
                        DeviceRowKind::Text {
                            value: if saved { "set".into() } else { String::new() },
                            secret: true,
                            limit: 63,
                        },
                        Row::Password,
                    );
                }
                if saved {
                    push(
                        "Forget this network".into(),
                        DeviceRowKind::Action,
                        Row::Forget,
                    );
                }
                ssid.clone()
            }
        };
        self.rows = actions;
        ui.set_device_view(&title, rows).map_err(|e| e.to_string())
    }
}
