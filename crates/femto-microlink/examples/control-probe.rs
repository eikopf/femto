//! Opt-in control transport probe. No registration or map request is sent.
#[cfg(all(unix, not(target_os = "espidf")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use femto_microlink::{
        host_control::ControlSession,
        protocol::{
            CAPABILITY_VERSION,
            identity::DeviceIdentity,
            key::{Machine, PublicKey},
        },
    };
    use std::{
        sync::{Arc, atomic::AtomicBool},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 && args.len() != 4 {
        return Err(
            "usage: control-probe HOSTNAME NUMERIC-IP:PORT [AUTHENTICATED-PUBLIC-CONTROL-KEY]"
                .into(),
        );
    }
    let started = Instant::now();
    let key = if let Some(key) = args.get(3) {
        PublicKey::<Machine>::parse(key)?
    } else {
        femto_microlink::host_bootstrap::fetch_control_key(
            &args[1],
            args[2].parse()?,
            CAPABILITY_VERSION,
            Duration::from_secs(10),
            Arc::new(AtomicBool::new(false)),
        )?
    };
    let identity = DeviceIdentity::generate()?;
    let mut control = ControlSession::connect(
        &args[1],
        args[2].parse()?,
        identity.machine_key(),
        key,
        CAPABILITY_VERSION,
        Duration::from_secs(10),
        Arc::new(AtomicBool::new(false)),
    )?;
    if control.challenge()? == PublicKey::zero() {
        return Err("missing node-key challenge".into());
    }
    control.close();
    println!(
        "{}",
        serde_json::json!({"result":"passed","host":args[1],"address":args[2],"unix_seconds":SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),"elapsed_ms":started.elapsed().as_millis(),"capability_version":CAPABILITY_VERSION,"bootstrap":if args.len() == 3 { "Rust verified HTTPS" } else { "caller-supplied key" },"verified":"TLS, Noise IK, EarlyNoise challenge, HTTP2 settings","tailnet_enrolled":false})
    );
    Ok(())
}
#[cfg(not(all(unix, not(target_os = "espidf"))))]
fn main() {
    eprintln!("This diagnostic requires the Unix host TLS adapter.");
}
