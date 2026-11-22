//! Service settings edited on the device and kept in internal storage.
//!
//! Each service's settings are one JSON document in the same shape its
//! parser already accepted from the SD card, so the existing validation
//! decides whether a draft is complete. Drafts are stored as typed; a
//! service starts only from settings that validate.
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

type Secret = Zeroizing<String>;

pub enum Kind {
    Text,
    Secret,
    Toggle(bool),
}

pub struct Field {
    pub label: &'static str,
    pub kind: Kind,
    /// Current text; for secrets only whether one is set.
    pub value: String,
    pub limit: usize,
}

fn text(label: &'static str, value: &str, limit: usize) -> Field {
    Field {
        label,
        kind: Kind::Text,
        value: value.into(),
        limit,
    }
}
fn secret(label: &'static str, set: bool, limit: usize) -> Field {
    Field {
        label,
        kind: Kind::Secret,
        value: if set { "set".into() } else { String::new() },
        limit,
    }
}
fn toggle(label: &'static str, on: bool) -> Field {
    Field {
        label,
        kind: Kind::Toggle(on),
        value: String::new(),
        limit: 0,
    }
}

/// One service's editable settings.
pub trait Form {
    fn title(&self) -> &'static str;
    fn fields(&self) -> Vec<Field>;
    fn set_text(&mut self, field: usize, value: String);
    fn toggle(&mut self, field: usize);
    /// Whether the service could start from these settings.
    fn check(&self) -> Result<(), String>;
    fn store(&self) -> Result<(), String>;
}

fn load<T: for<'a> Deserialize<'a> + Default>(key: &str) -> T {
    match crate::nvs::get(key) {
        Ok(Some(bytes)) => serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            log::warn!("Stored {key} settings are unreadable; starting empty");
            T::default()
        }),
        Ok(None) => T::default(),
        Err(error) => {
            log::warn!("Stored {key} settings unavailable: {error}");
            T::default()
        }
    }
}

fn bytes<T: Serialize>(settings: &T) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(serde_json::to_vec(settings).expect("settings serialize"))
}

fn save<T: Serialize>(key: &str, settings: &T) -> Result<(), String> {
    crate::nvs::set(key, &bytes(settings))
}

// --- KOReader sync ---------------------------------------------------------

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Kosync {
    #[serde(default = "default_sync_server")]
    server: String,
    #[serde(default)]
    username: Secret,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password: Option<Secret>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    userkey: Option<Secret>,
    #[serde(default)]
    allow_http: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checksum: Option<String>,
    #[serde(default = "default_route")]
    route: String,
    #[serde(default = "default_device")]
    device: String,
    #[serde(default)]
    device_id: String,
}
fn default_sync_server() -> String {
    femto_kosync::DEFAULT_SERVER.into()
}
fn default_route() -> String {
    "lan".into()
}
fn default_device() -> String {
    "Femto".into()
}
impl Default for Kosync {
    fn default() -> Self {
        Self {
            server: default_sync_server(),
            username: Secret::default(),
            password: None,
            userkey: None,
            allow_http: false,
            checksum: None,
            route: default_route(),
            device: default_device(),
            device_id: String::new(),
        }
    }
}
impl Kosync {
    pub const KEY: &'static str = "kosync";
    pub fn load() -> Self {
        load(Self::KEY)
    }
    pub fn uses_tailnet(&self) -> bool {
        self.route == "tailnet"
    }
    pub fn settings(&self) -> Result<femto_kosync::config::Settings, String> {
        femto_kosync::config::Settings::from_json(&bytes(self)).map_err(|e| e.to_string())
    }
}
impl Form for Kosync {
    fn title(&self) -> &'static str {
        "KOReader sync"
    }
    fn fields(&self) -> Vec<Field> {
        vec![
            text("Server", &self.server, 200),
            text("Username", &self.username, 64),
            secret(
                "Password",
                self.password.is_some() || self.userkey.is_some(),
                64,
            ),
            toggle("Allow plain HTTP", self.allow_http),
            toggle("Reach through tailnet", self.route == "tailnet"),
        ]
    }
    fn set_text(&mut self, field: usize, value: String) {
        match field {
            0 => self.server = value,
            1 => self.username = Secret::new(value),
            2 => {
                self.userkey = None;
                self.password = (!value.is_empty()).then(|| Secret::new(value));
            }
            _ => {}
        }
    }
    fn toggle(&mut self, field: usize) {
        match field {
            3 => self.allow_http = !self.allow_http,
            4 => {
                self.route = if self.route == "tailnet" {
                    "lan"
                } else {
                    "tailnet"
                }
                .into()
            }
            _ => {}
        }
    }
    fn check(&self) -> Result<(), String> {
        self.settings().map(|_| ())
    }
    fn store(&self) -> Result<(), String> {
        save(Self::KEY, self)
    }
}

/// A device identifier for sync records, created once.
fn device_id() -> String {
    let mut random = [0u8; 16];
    unsafe { esp_idf_svc::sys::esp_fill_random(random.as_mut_ptr().cast(), random.len()) };
    random.iter().map(|byte| format!("{byte:02X}")).collect()
}

// --- Catalog ---------------------------------------------------------------

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Catalog {
    url: String,
    username: Secret,
    password: Secret,
    allow_http: bool,
    /// Origins fetched through the tailnet instead of the local network.
    tailnet_origins: Vec<String>,
}
impl Catalog {
    pub const KEY: &'static str = "catalog";
    pub fn load() -> Self {
        load(Self::KEY)
    }
    fn origin(&self) -> Option<String> {
        femto_catalog::http::Url::parse(&self.url)
            .ok()
            .map(|url| url.origin().ascii_serialization())
    }
    pub fn uses_tailnet(&self) -> bool {
        !self.tailnet_origins.is_empty()
    }
    /// The catalog menu for these settings; `None` without an address.
    pub fn menu(&self) -> Result<Option<crate::catalog::CatalogMenu>, String> {
        if self.url.is_empty() {
            return Ok(None);
        }
        let auth = if self.username.is_empty() && self.password.is_empty() {
            None
        } else {
            Some(
                femto_catalog::auth::BasicAuth::new(
                    &self.url,
                    &self.username,
                    &self.password,
                    self.allow_http,
                )
                .map_err(|e| e.to_string())?,
            )
        };
        crate::catalog::CatalogMenu::configured(
            self.url.clone(),
            self.tailnet_origins.clone(),
            auth,
            "/sdcard/femto".into(),
        )
        .map(Some)
    }
}
impl Form for Catalog {
    fn title(&self) -> &'static str {
        "Catalog"
    }
    fn fields(&self) -> Vec<Field> {
        vec![
            text("Address", &self.url, 200),
            text("Username", &self.username, 64),
            secret("Password", !self.password.is_empty(), 64),
            toggle("Allow plain HTTP", self.allow_http),
            toggle("Reach through tailnet", !self.tailnet_origins.is_empty()),
        ]
    }
    fn set_text(&mut self, field: usize, value: String) {
        match field {
            0 => {
                self.url = value;
                // Routing follows the address.
                if !self.tailnet_origins.is_empty() {
                    self.tailnet_origins = self.origin().into_iter().collect();
                }
            }
            1 => self.username = Secret::new(value),
            2 => self.password = Secret::new(value),
            _ => {}
        }
    }
    fn toggle(&mut self, field: usize) {
        match field {
            3 => self.allow_http = !self.allow_http,
            4 => {
                self.tailnet_origins = if self.tailnet_origins.is_empty() {
                    self.origin().into_iter().collect()
                } else {
                    Vec::new()
                }
            }
            _ => {}
        }
    }
    fn check(&self) -> Result<(), String> {
        match self.menu()? {
            Some(_) => Ok(()),
            None => Err("No address".into()),
        }
    }
    fn store(&self) -> Result<(), String> {
        save(Self::KEY, self)
    }
}

// --- Tailnet ---------------------------------------------------------------

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tailnet {
    version: u8,
    host: String,
    port: u16,
    hostname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth_key: Option<Secret>,
}
impl Default for Tailnet {
    fn default() -> Self {
        Self {
            version: 1,
            host: "controlplane.tailscale.com".into(),
            port: 443,
            hostname: "femto-picco".into(),
            auth_key: None,
        }
    }
}
impl Tailnet {
    pub const KEY: &'static str = "tailnet";
    pub fn load() -> Self {
        load(Self::KEY)
    }
    pub fn config(&self) -> Result<femto_microlink::device_config::Config, String> {
        femto_microlink::device_config::Config::parse(&bytes(self)).map_err(Into::into)
    }
}
impl Form for Tailnet {
    fn title(&self) -> &'static str {
        "Tailnet"
    }
    fn fields(&self) -> Vec<Field> {
        vec![
            text("Coordinator", &self.host, 128),
            text("Port", &self.port.to_string(), 5),
            text("Device name", &self.hostname, 63),
            secret("Auth key", self.auth_key.is_some(), 256),
        ]
    }
    fn set_text(&mut self, field: usize, value: String) {
        match field {
            0 => self.host = value,
            // An unparsable port keeps the previous one.
            1 => self.port = value.trim().parse().unwrap_or(self.port),
            2 => self.hostname = value,
            3 => self.auth_key = (!value.is_empty()).then(|| Secret::new(value)),
            _ => {}
        }
    }
    fn toggle(&mut self, _: usize) {}
    fn check(&self) -> Result<(), String> {
        self.config().map(|_| ())
    }
    fn store(&self) -> Result<(), String> {
        save(Self::KEY, self)
    }
}

pub fn forms() -> [Box<dyn Form>; 3] {
    let mut kosync = Kosync::load();
    if kosync.device_id.is_empty() {
        kosync.device_id = device_id();
    }
    [
        Box::new(Catalog::load()),
        Box::new(kosync),
        Box::new(Tailnet::load()),
    ]
}

/// Moves service settings found on the SD card into internal storage, once,
/// and deletes the files so that credentials stop living on removable media.
pub fn import_from_card() {
    let read = |name: &str| {
        std::fs::read(format!("/sdcard/femto/{name}"))
            .ok()
            .map(Zeroizing::new)
    };
    let remove = |name: &str| {
        if let Err(error) = std::fs::remove_file(format!("/sdcard/femto/{name}")) {
            log::warn!("Cannot remove {name} from the card: {error}");
        }
    };
    let stored = |key: &str| matches!(crate::nvs::get(key), Ok(Some(_)));
    let moved = |key: &str, result: Result<(), String>| match result {
        Ok(()) => {
            log::info!("Moved {key} settings from the card into internal storage");
            true
        }
        Err(error) => {
            log::warn!("Cannot move {key} settings from the card: {error}");
            false
        }
    };
    for (key, file) in [(Kosync::KEY, "kosync.json"), (Tailnet::KEY, "tailnet.json")] {
        let Some(bytes) = read(file) else { continue };
        // The card's copy wins only where nothing is stored yet.
        if stored(key) || moved(key, crate::nvs::set(key, &bytes)) {
            remove(file);
        }
    }
    if let Some(url) = read("catalog.url") {
        let mut catalog = Catalog {
            url: String::from_utf8_lossy(&url).trim().to_owned(),
            ..Catalog::default()
        };
        #[derive(Deserialize)]
        struct Auth {
            username: Secret,
            password: Secret,
            #[serde(default)]
            allow_http: bool,
        }
        if let Some(auth) =
            read("catalog-auth.json").and_then(|bytes| serde_json::from_slice::<Auth>(&bytes).ok())
        {
            catalog.username = auth.username;
            catalog.password = auth.password;
            catalog.allow_http = auth.allow_http;
        }
        if let Some(origins) = read("catalog-tailnet-origins") {
            catalog.tailnet_origins = String::from_utf8_lossy(&origins)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect();
        }
        if stored(Catalog::KEY) || moved(Catalog::KEY, catalog.store()) {
            for file in [
                "catalog.url",
                "catalog-auth.json",
                "catalog-tailnet-origins",
            ] {
                if read(file).is_some() {
                    remove(file);
                }
            }
        }
    }
}
