//! Independent coordinator fixture for the application registration/runtime path.
use femto_microlink::{
    host_registration::{Connection, Options, connect_with_tls},
    host_tcp::Driver,
    host_tls::TlsConnector,
    protocol::control::HostInfo,
    store::HostStore,
};
use std::{
    fs, io,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        return Err("expected address root.der auth-file identity-directory".into());
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.add(rustls::pki_types::CertificateDer::from(fs::read(&args[2])?))?;
    let tls = TlsConnector::with_roots(roots, Duration::from_secs(60))?;
    let auth = zeroize::Zeroizing::new(fs::read_to_string(&args[3])?);
    let options = Options {
        host: "localhost".into(),
        address: args[1].parse()?,
        pinned_key: None,
        host_info: HostInfo::esp32("femto-independent-probe", ""),
        auth_key: Some(auth.trim().into()),
        followup: None,
        rotate_node_key: false,
    };
    let Connection::Connected(mut runtime) = connect_with_tls(
        HostStore::open(&args[4])?,
        options,
        "127.0.0.1:0".parse()?,
        Arc::new(AtomicBool::new(false)),
        tls,
    )?
    else {
        return Err("registration not authorized".into());
    };
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut assigned = None;
    let mut revoked = false;
    while Instant::now() < deadline {
        match runtime.pump() {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
            Err(error) => return Err(error.into()),
        }
        let current = runtime
            .run(|network, now| network.local_address(now))
            .ok()
            .flatten();
        match (assigned, current) {
            (None, Some(address)) => {
                assigned = Some(address);
                println!("application runtime map ready");
            }
            (Some(_), None) if !revoked => {
                revoked = true;
                println!("application runtime map revoked");
            }
            (Some(previous), Some(address)) if revoked => {
                if previous != address {
                    return Err("reconnect changed assigned address".into());
                }
                runtime.close();
                if runtime.run(|_, _| Ok(())).is_ok() {
                    return Err("closed runtime allowed access".into());
                }
                drop(runtime);
                let _store = HostStore::open(&args[4])?;
                println!(
                    "{}",
                    serde_json::json!({"application_registration":true,"application_map":true,"application_reconnect":true,"revoked_during_restart":true,"closed_runtime_denies_access":true,"identity_lock_released":true,"assigned_ipv4":address})
                );
                return Ok(());
            }
            _ => (),
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err("application runtime reconnection timed out".into())
}
