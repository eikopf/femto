use crate::{Client, Credentials, DEFAULT_SERVER, Error};
use serde::Deserialize;
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Checksum {
    #[default]
    Binary,
    Filename,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    #[default]
    Lan,
    Tailnet,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub client: Client,
    pub checksum: Checksum,
    pub route: Route,
    pub device: String,
    pub device_id: String,
}
impl Settings {
    pub fn load(path: &std::path::Path) -> Result<Self, Error> {
        use std::io::Read;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(all(unix, not(target_os = "espidf")))]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(Error::Invalid("configuration must be a regular file"));
        }
        #[cfg(all(unix, not(target_os = "espidf")))]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(Error::Invalid(
                    "configuration must be a private regular file",
                ));
            }
        }
        let mut bytes = Zeroizing::new(Vec::new());
        file.take(8193).read_to_end(&mut bytes)?;
        Self::from_json(&bytes)
    }
    /// The storage owner enforces private-file permissions where supported.
    /// Parse errors exclude credential values and serde's input excerpts.
    pub fn from_json(bytes: &[u8]) -> Result<Self, Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            #[serde(default = "server")]
            server: String,
            username: Zeroizing<String>,
            password: Option<Zeroizing<String>>,
            userkey: Option<Zeroizing<String>>,
            #[serde(default)]
            allow_http: bool,
            #[serde(default)]
            checksum: Checksum,
            #[serde(default)]
            route: Route,
            #[serde(default = "device")]
            device: String,
            device_id: String,
        }
        fn server() -> String {
            DEFAULT_SERVER.into()
        }
        fn device() -> String {
            "Femto".into()
        }
        if bytes.len() > 8192 {
            return Err(Error::Invalid("configuration size"));
        }
        let input: Input =
            serde_json::from_slice(bytes).map_err(|_| Error::Invalid("configuration JSON"))?;
        let credentials = match (&input.password, &input.userkey) {
            (Some(password), None) => Credentials::from_password(&input.username, password)?,
            (None, Some(key)) => Credentials::from_key(&input.username, key)?,
            _ => return Err(Error::Invalid("configuration: specify password or userkey")),
        };
        if !super::text(&input.device, 256) || !super::text(&input.device_id, 256) {
            return Err(Error::Invalid("device identity"));
        }
        Ok(Self {
            client: Client::new(&input.server, credentials, input.allow_http)?,
            checksum: input.checksum,
            route: input.route,
            device: input.device,
            device_id: input.device_id,
        })
    }
}
