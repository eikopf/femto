use esp_idf_svc::{
    bt::{
        BdAddr, Ble, BtDriver, BtUuid,
        ble::{
            gap::BleAddrType,
            gatt::{
                GattStatus, Property,
                client::{
                    CharacteristicElement, DbAttrType, DescriptorElement, EspGattc, GattAuthReq,
                    GattWriteType, GattcEvent,
                },
            },
        },
    },
    hal::modem::BluetoothModem,
};
use femto_reader::{
    remote::{PageTurn, RemoteControl, SessionToken, Submit},
    state::ReaderState,
};
use femto_remote::discovery::{
    AddressType, Characteristic, CharacteristicKind, Command, Connection, Discovery,
    HidCharacteristics, MAX_CHARACTERISTICS, MAX_MAP_BYTES, MAX_PEER_BYTES, Peer, ReadKind,
    ReadRequest, ReadResponse, ServiceRange,
};
use femto_remote::{
    RemoteInput,
    gatt::{
        Descriptor, DescriptorKind, InputReport, MAX_DESCRIPTORS, ReportDescriptors, ReportRoutes,
    },
    shutdown::{Command as ShutdownCommand, Shutdown},
    subscriptions::{Command as SubscribeCommand, Subscriptions},
};
use std::{
    fs::File,
    io::Read,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const APP_ID: u16 = 42;
const HID: BtUuid = BtUuid::uuid16(0x1812);
// Reject SDK-buffer-sized results before a truncated map can reach the parser.
const _: () = assert!(esp_idf_svc::sys::ESP_GATT_MAX_ATTR_LEN as usize > MAX_MAP_BYTES);
type Client = EspGattc<'static, Ble, Arc<BtDriver<'static, Ble>>>;

pub(super) fn load_peer() -> Result<Option<Peer>, String> {
    let path = "/sdcard/femto/bluetooth-peer";
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Bluetooth peer configuration: {error}")),
    };
    if !metadata.is_file() || metadata.len() > MAX_PEER_BYTES as u64 {
        return Err(
            "Bluetooth peer configuration must be a regular file of at most 64 bytes".into(),
        );
    }
    let mut bytes = [0; MAX_PEER_BYTES + 1];
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut len = 0;
    while len < bytes.len() {
        let read = file.read(&mut bytes[len..]).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        len += read;
    }
    Peer::parse(&bytes[..len]).map(Some).map_err(str::to_owned)
}

pub struct Probe {
    client: Client,
    state: Arc<Mutex<State>>,
    deadline: Instant,
    plan: Option<ReadPlan>,
    peer: Peer,
    stop_deadline: Option<Instant>,
}

struct State {
    shutdown: Shutdown,
    discovery: Discovery,
    remote: RemoteControl,
    token: Option<SessionToken>,
    subscriptions: Option<Subscriptions>,
    input_ready: bool,
    peer: Peer,
}

impl State {
    fn error(&self) -> Option<&'static str> {
        self.discovery
            .error()
            .or(self.subscriptions.as_ref().and_then(Subscriptions::error))
    }
    fn invalidate_input(&mut self) {
        if let Some(token) = self.token.take() {
            self.remote.disconnect(token);
        }
    }
    fn synchronize(&mut self) {
        if let Some(error) = self.error() {
            self.discovery.abort(error);
            self.invalidate_input();
        }
    }
    fn abort(&mut self, error: &'static str) {
        self.discovery.abort(error);
        if let Some(subscriptions) = &mut self.subscriptions {
            subscriptions.cancel();
        }
        self.invalidate_input();
    }
    fn ready(&self) -> bool {
        self.discovery.is_done()
            && self.error().is_none()
            && self
                .subscriptions
                .as_ref()
                .is_some_and(Subscriptions::is_ready)
    }
}

struct ReadPlan {
    connection: Connection,
    map_handle: u16,
    input: Option<RemoteInput>,
    reports: [InputReport; 8],
    references: [u16; 8],
    len: usize,
    next_reference: usize,
}

impl Probe {
    pub fn open(modem: BluetoothModem<'static>, peer: Peer) -> Result<Self, String> {
        let driver = Arc::new(BtDriver::new(modem, None).map_err(|e| e.to_string())?);
        let client = Client::new(driver).map_err(|e| e.to_string())?;
        let state = Arc::new(Mutex::new(State {
            shutdown: Shutdown::default(),
            discovery: Discovery::new(peer),
            remote: RemoteControl::default(),
            token: None,
            subscriptions: None,
            input_ready: false,
            peer,
        }));
        let callbacks = state.clone();
        client
            .subscribe(move |(interface, event)| {
                let mut state = callbacks.lock().unwrap_or_else(|error| error.into_inner());
                match &event {
                    GattcEvent::ClientRegistered { status, app_id } if *app_id == APP_ID => state
                        .shutdown
                        .registered(interface, *status == GattStatus::Ok),
                    GattcEvent::Open {
                        status, conn_id, ..
                    } => state.shutdown.opened(
                        Connection {
                            interface,
                            id: *conn_id,
                        },
                        *status == GattStatus::Ok,
                    ),
                    GattcEvent::Close {
                        status, conn_id, ..
                    } => state.shutdown.closed(
                        Connection {
                            interface,
                            id: *conn_id,
                        },
                        *status == GattStatus::Ok,
                    ),
                    GattcEvent::Disconnected { conn_id, .. } => state.shutdown.closed(
                        Connection {
                            interface,
                            id: *conn_id,
                        },
                        true,
                    ),
                    GattcEvent::ClientUnregistered => state.shutdown.unregistered(interface),
                    _ => {}
                }
                if state.shutdown.is_stopping() {
                    return;
                }
                match event {
                    GattcEvent::ClientRegistered { status, app_id } if app_id == APP_ID => {
                        state
                            .discovery
                            .registered(interface, status == GattStatus::Ok);
                    }
                    GattcEvent::Open {
                        status,
                        conn_id,
                        addr,
                        ..
                    } => {
                        state.discovery.opened(
                            Connection {
                                interface,
                                id: conn_id,
                            },
                            addr.addr(),
                            status == GattStatus::Ok,
                        );
                    }
                    GattcEvent::SearchResult {
                        conn_id,
                        start_handle,
                        end_handle,
                        srvc_id,
                        is_primary,
                    } if srvc_id.uuid == HID => {
                        state.discovery.hid_service(
                            Connection {
                                interface,
                                id: conn_id,
                            },
                            ServiceRange {
                                start: start_handle,
                                end: end_handle,
                            },
                            is_primary,
                        );
                    }
                    GattcEvent::SearchComplete {
                        status, conn_id, ..
                    } => {
                        state.discovery.searched(
                            Connection {
                                interface,
                                id: conn_id,
                            },
                            status == GattStatus::Ok,
                        );
                    }
                    GattcEvent::Disconnected { conn_id, .. }
                    | GattcEvent::Close { conn_id, .. } => {
                        state.discovery.disconnected(Connection {
                            interface,
                            id: conn_id,
                        });
                    }
                    GattcEvent::ServiceChanged { addr } => {
                        state.discovery.service_changed(addr.addr())
                    }
                    GattcEvent::RegisterNotify { status, handle } => {
                        if let Some(subscriptions) = &mut state.subscriptions {
                            subscriptions.registered(interface, handle, status == GattStatus::Ok);
                        }
                    }
                    GattcEvent::WriteDescriptor {
                        status,
                        conn_id,
                        handle,
                    } => {
                        if let Some(subscriptions) = &mut state.subscriptions {
                            subscriptions.configured(
                                Connection {
                                    interface,
                                    id: conn_id,
                                },
                                handle,
                                status == GattStatus::Ok,
                            );
                        }
                    }
                    GattcEvent::Notify {
                        conn_id,
                        addr,
                        handle,
                        value,
                        is_notify,
                    } => {
                        if state.ready()
                            && state.input_ready
                            && state.peer.address() == addr.addr()
                            && state.discovery.connection()
                                == Some(Connection {
                                    interface,
                                    id: conn_id,
                                })
                        {
                            if !is_notify {
                                state.abort("unexpected HID indication");
                            } else if let Some(token) = state.token {
                                if state.remote.submit_gatt(token, handle, value)
                                    == Submit::Invalidated
                                {
                                    state.abort("HID input queue invalidated");
                                }
                            }
                        }
                    }
                    GattcEvent::ReadCharacteristic {
                        status,
                        conn_id,
                        handle,
                        value,
                    } => {
                        state.discovery.read_completed(
                            ReadRequest {
                                connection: Connection {
                                    interface,
                                    id: conn_id,
                                },
                                handle,
                                kind: ReadKind::ReportMap,
                            },
                            status == GattStatus::Ok,
                            value.unwrap_or_default(),
                        );
                    }
                    GattcEvent::ReadDescriptor {
                        status,
                        conn_id,
                        handle,
                        value,
                    } => {
                        state.discovery.read_completed(
                            ReadRequest {
                                connection: Connection {
                                    interface,
                                    id: conn_id,
                                },
                                handle,
                                kind: ReadKind::ReportReference,
                            },
                            status == GattStatus::Ok,
                            value.unwrap_or_default(),
                        );
                    }
                    GattcEvent::QueueFull {
                        conn_id,
                        is_full: true,
                        ..
                    } if state.discovery.connection()
                        == Some(Connection {
                            interface,
                            id: conn_id,
                        }) =>
                    {
                        state.abort("GATT command queue full");
                    }
                    GattcEvent::ClientUnregistered
                        if state.discovery.interface() == Some(interface) =>
                    {
                        state.abort("GATT client unregistered");
                    }
                    _ => {}
                }
                state.synchronize();
            })
            .map_err(|e| e.to_string())?;
        let probe = Self {
            client,
            state,
            deadline: Instant::now() + Duration::from_secs(15),
            plan: None,
            peer,
            stop_deadline: None,
        };
        probe
            .client
            .register_app(APP_ID)
            .map_err(|e| e.to_string())?;
        Ok(probe)
    }

    pub fn is_ready(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).ready()
    }

    pub fn stop(&mut self) {
        if self.stop_deadline.is_none() {
            self.stop_deadline = Some(Instant::now() + Duration::from_secs(15));
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.abort("GATT stopping");
            state.shutdown.stop();
        }
    }

    pub fn poll(&mut self) -> Result<bool, String> {
        if self.stop_deadline.is_none() {
            match self.poll_active() {
                Ok(result) => return Ok(result),
                Err(error) => {
                    log::warn!("Bluetooth stopping: {error}");
                    self.stop();
                }
            }
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.shutdown.is_done() {
            return Ok(true);
        }
        if self
            .stop_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err("GATT shutdown acknowledgement deadline expired".into());
        }
        let command = state.shutdown.take_command();
        drop(state);
        match command {
            Some(ShutdownCommand::Close(connection)) => {
                self.client.close(connection.interface, connection.id)
            }
            Some(ShutdownCommand::Unregister(interface)) => self.client.unregister_app(interface),
            None => return Ok(false),
        }
        .map_err(|e| format!("GATT shutdown rejected: {e}"))?;
        Ok(false)
    }

    fn poll_active(&mut self) -> Result<bool, String> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(error) = state.error() {
            return Err(error.into());
        }
        if state.ready() {
            return Ok(false);
        }
        if Instant::now() >= self.deadline {
            state.abort("GATT discovery deadline expired");
            return Err("GATT discovery deadline expired".into());
        }
        let command = state.discovery.take_command();
        if let Some(Command::Open { interface, .. }) = command {
            if !state.shutdown.opening(interface) {
                return Err("GATT open outside active lifecycle".into());
            }
        }
        let response = state.discovery.take_read().map_err(str::to_owned)?;
        let subscription = state
            .subscriptions
            .as_mut()
            .and_then(Subscriptions::take_command);
        let connection = state.discovery.connection();
        drop(state);
        if let Some(command) = command {
            self.deadline = Instant::now() + Duration::from_secs(15);
            match command {
                Command::Open { interface, peer } => {
                    if let Err(error) = self.client.open(
                        interface,
                        &BdAddr::from_bytes(peer.address()),
                        match peer.address_type() {
                            AddressType::Public => BleAddrType::Public,
                            AddressType::Random => BleAddrType::Random,
                        },
                        true,
                    ) {
                        self.state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .shutdown
                            .open_rejected();
                        return Err(error.to_string());
                    }
                }
                Command::SearchHid(connection) => self
                    .client
                    .search_service(connection.interface, connection.id, Some(&HID))
                    .map_err(|e| e.to_string())?,
                Command::Inspect {
                    connection,
                    service,
                } => self.inspect(connection, service)?,
            }
        }
        if let Some(response) = response {
            self.accept_read(response)?;
        }
        if let Some(command) = subscription {
            let connection = connection.ok_or("missing subscription connection")?;
            self.deadline = Instant::now() + Duration::from_secs(15);
            match command {
                SubscribeCommand::Register(handle) => self.client.register_for_notify(
                    connection.interface,
                    &BdAddr::from_bytes(self.peer.address()),
                    handle,
                ),
                SubscribeCommand::Enable(handle) => self.client.write_descriptor(
                    connection.interface,
                    connection.id,
                    handle,
                    &[1, 0],
                    GattWriteType::RequireResponse,
                    GattAuthReq::None,
                ),
            }
            .map_err(|error| error.to_string())?;
        }
        Ok(false)
    }

    pub fn poll_input(
        &mut self,
        reader: &ReaderState,
        allow_turn: bool,
    ) -> Result<Option<PageTurn>, String> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.shutdown.is_stopping() {
            return Ok(None);
        }
        state.input_ready = true;
        state.synchronize();
        if let Some(error) = state.error() {
            log::warn!("Bluetooth stopping: {error}");
            drop(state);
            self.stop();
            return Ok(None);
        }
        if !state.ready() {
            return Ok(None);
        }
        match state.remote.poll_action(reader, allow_turn) {
            Ok(action) => Ok(action),
            Err(error) => {
                state.abort("HID input decode failed");
                drop(state);
                log::warn!("Bluetooth input: {error}");
                self.stop();
                Ok(None)
            }
        }
    }

    fn inspect(&mut self, connection: Connection, service: ServiceRange) -> Result<(), String> {
        let count = self
            .client
            .get_attr_count(
                connection.interface,
                connection.id,
                DbAttrType::Characteristic {
                    start_handle: service.start,
                    end_handle: service.end,
                },
            )
            .map_err(|e| format!("GATT characteristic count: {e:?}"))?;
        if count == 0 || count > MAX_CHARACTERISTICS {
            return Err("HID characteristic count outside 1..=32".into());
        }
        let mut characteristics = [CharacteristicElement::new(); MAX_CHARACTERISTICS];
        let received = self
            .client
            .get_all_characteristics(
                connection.interface,
                connection.id,
                service.start,
                service.end,
                0,
                &mut characteristics[..count],
            )
            .map_err(|e| format!("GATT characteristic discovery: {e:?}"))?;
        if received != count {
            return Err("GATT characteristic count changed".into());
        }
        characteristics[..count].sort_unstable_by_key(CharacteristicElement::handle);
        let mut summaries = [Characteristic::default(); MAX_CHARACTERISTICS];
        for (summary, characteristic) in summaries.iter_mut().zip(&characteristics[..count]) {
            *summary = Characteristic {
                handle: characteristic.handle(),
                kind: if characteristic.uuid() == BtUuid::uuid16(0x2a4b) {
                    CharacteristicKind::ReportMap
                } else if characteristic.uuid() == BtUuid::uuid16(0x2a4d) {
                    CharacteristicKind::Report
                } else {
                    CharacteristicKind::Other
                },
                readable: characteristic.properties().contains(Property::Read),
                notifying: characteristic.properties().contains(Property::Notify),
            };
        }
        let handles =
            HidCharacteristics::new(service, &summaries[..count]).map_err(str::to_owned)?;
        let mut plan = ReadPlan {
            connection,
            map_handle: handles.map_handle(),
            input: None,
            reports: [InputReport::default(); 8],
            references: [0; 8],
            len: handles.report_handles().len(),
            next_reference: 0,
        };
        for (index, &value_handle) in handles.report_handles().iter().enumerate() {
            let position = characteristics[..count]
                .iter()
                .position(|entry| entry.handle() == value_handle)
                .ok_or("missing report characteristic")?;
            let last_handle = characteristics[..count]
                .get(position + 1)
                .map(|entry| entry.handle() - 1)
                .unwrap_or(service.end);
            let descriptors = self.descriptor_handles(connection, value_handle, last_handle)?;
            plan.reports[index] = InputReport {
                value_handle,
                cccd_handle: descriptors.configuration_handle(),
                reference: [0; 2],
            };
            plan.references[index] = descriptors.reference_handle();
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(error) = state.error() {
            return Err(error.into());
        }
        state.discovery.inspected(connection);
        if !state.discovery.is_discovered() {
            return Err("GATT discovery invalidated".into());
        }
        drop(state);
        log::info!(
            "HID service {}..={} has {count} characteristics, Report Map {}, and {} notifying reports",
            service.start,
            service.end,
            handles.map_handle(),
            handles.report_handles().len()
        );
        for characteristic in &characteristics[..count] {
            log::info!("HID characteristic: {characteristic:?}");
        }
        self.plan = Some(plan);
        self.start_read(ReadRequest {
            connection,
            handle: handles.map_handle(),
            kind: ReadKind::ReportMap,
        })?;
        Ok(())
    }
    fn descriptor_handles(
        &self,
        connection: Connection,
        value_handle: u16,
        last_handle: u16,
    ) -> Result<ReportDescriptors, String> {
        let count = self
            .client
            .get_attr_count(
                connection.interface,
                connection.id,
                DbAttrType::Descriptor {
                    handle: value_handle,
                },
            )
            .map_err(|error| format!("GATT descriptor count: {error:?}"))?;
        if count == 0 || count > MAX_DESCRIPTORS {
            return Err("GATT descriptor count outside 1..=16".into());
        }
        let mut descriptors = [DescriptorElement::new(); MAX_DESCRIPTORS];
        let received = self
            .client
            .get_all_descriptors(
                connection.interface,
                connection.id,
                value_handle,
                0,
                &mut descriptors[..count],
            )
            .map_err(|error| format!("GATT descriptors: {error:?}"))?;
        if received != count {
            return Err("GATT descriptor count changed".into());
        }
        descriptors[..count].sort_unstable_by_key(DescriptorElement::handle);
        let mut summaries = [Descriptor::default(); MAX_DESCRIPTORS];
        for (summary, descriptor) in summaries.iter_mut().zip(&descriptors[..count]) {
            *summary = Descriptor {
                handle: descriptor.handle(),
                kind: if descriptor.uuid() == BtUuid::uuid16(0x2908) {
                    DescriptorKind::ReportReference
                } else if descriptor.uuid() == BtUuid::uuid16(0x2902) {
                    DescriptorKind::ClientConfiguration
                } else {
                    DescriptorKind::Other
                },
            };
        }
        ReportDescriptors::new(value_handle, last_handle, &summaries[..count])
            .map_err(str::to_owned)
    }

    fn start_read(&mut self, request: ReadRequest) -> Result<(), String> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .discovery
            .begin_read(request)
            .map_err(str::to_owned)?;
        self.deadline = Instant::now() + Duration::from_secs(15);
        match request.kind {
            ReadKind::ReportMap => self.client.read_characteristic(
                request.connection.interface,
                request.connection.id,
                request.handle,
                GattAuthReq::None,
            ),
            ReadKind::ReportReference => self.client.read_descriptor(
                request.connection.interface,
                request.connection.id,
                request.handle,
                GattAuthReq::None,
            ),
        }
        .map_err(|error| error.to_string())
    }

    fn accept_read(&mut self, response: ReadResponse) -> Result<(), String> {
        let plan = self.plan.as_mut().ok_or("missing HID read plan")?;
        if response.request.connection != plan.connection {
            return Err("HID read connection changed".into());
        }
        match response.request.kind {
            ReadKind::ReportMap => {
                if response.request.handle != plan.map_handle || plan.input.is_some() {
                    return Err("unexpected HID Report Map".into());
                }
                plan.input = Some(RemoteInput::new(response.bytes()).map_err(str::to_owned)?);
            }
            ReadKind::ReportReference => {
                if plan.input.is_none()
                    || plan.next_reference >= plan.len
                    || response.request.handle != plan.references[plan.next_reference]
                {
                    return Err("unexpected HID Report Reference".into());
                }
                let reference: [u8; 2] = response
                    .bytes()
                    .try_into()
                    .map_err(|_| "invalid Report Reference size")?;
                if reference[1] != 1 {
                    return Err("notifying report is not an input report".into());
                }
                plan.reports[plan.next_reference].reference = reference;
                plan.next_reference += 1;
            }
        }
        if plan.next_reference < plan.len {
            let request = ReadRequest {
                connection: plan.connection,
                handle: plan.references[plan.next_reference],
                kind: ReadKind::ReportReference,
            };
            return self.start_read(request);
        }
        let routes = ReportRoutes::new(
            plan.input.as_ref().ok_or("missing parsed Report Map")?,
            &plan.reports[..plan.len],
        )
        .map_err(str::to_owned)?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.discovery.finish(plan.connection);
        if !state.discovery.is_done() {
            return Err(state.error().unwrap_or("HID reads invalidated").into());
        }
        let input = plan.input.take().ok_or("missing parsed Report Map")?;
        let token = state
            .remote
            .connect_gatt_input(input, &plan.reports[..plan.len])
            .map_err(|error| error.to_string())?;
        state.token = Some(token);
        state.subscriptions = Some(Subscriptions::new(plan.connection, &routes));
        drop(state);
        self.deadline = Instant::now() + Duration::from_secs(15);
        log::info!(
            "Validated HID Report Map and {} input report routes",
            routes.reports().len()
        );
        Ok(())
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.abort("GATT discovery stopped");
        if state.shutdown.is_done() {
            drop(state);
            let _ = self.client.unsubscribe();
            return;
        }
        let connection = state.shutdown.connection();
        let interface = state.shutdown.interface();
        let subscribed = state.subscriptions.is_some();
        drop(state);
        if subscribed && let (Some(interface), Some(plan)) = (interface, self.plan.as_ref()) {
            for report in &plan.reports[..plan.len] {
                let _ = self.client.unregister_for_notify(
                    interface,
                    &BdAddr::from_bytes(self.peer.address()),
                    report.value_handle,
                );
            }
        }
        if let Some(connection) = connection {
            let _ = self.client.close(connection.interface, connection.id);
        }
        if let Some(interface) = interface {
            let _ = self.client.unregister_app(interface);
        }
        let _ = self.client.unsubscribe();
    }
}
