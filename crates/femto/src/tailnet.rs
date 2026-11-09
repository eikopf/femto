#[path = "tailnet_catalog.rs"]
mod catalog_runtime;
use anyhow::{Context, Result, bail};
use femto_microlink::host_reconnect as reconnect;
use femto_microlink::{
    coordinator::Clock,
    enrollment::{ApprovalUrl, Outcome},
    host_control_worker::ControlledNetwork,
    host_udp::UdpTransport,
    map_session::Progress,
    network::Network,
    protocol::{
        CAPABILITY_VERSION,
        control::{HostInfo, MapRequest},
        key::{Machine, PublicKey},
    },
    store::HostStore,
    timestamp::TimestampClock,
};
use std::{
    fs::{self, File},
    io::Read,
    net::{Ipv4Addr, SocketAddr},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(clap::Args)]
pub struct ConnectArgs {
    /// Private identity directory; its parent must exist.
    pub directory: PathBuf,
    /// TLS server name for the coordinator.
    #[arg(long, default_value = "controlplane.tailscale.com")]
    host: String,
    /// Numeric coordinator IP and port; TLS still verifies --host.
    #[arg(long)]
    endpoint: SocketAddr,
    /// Override automatic HTTPS bootstrap with an already authenticated mkey.
    #[arg(long)]
    control_key: Option<String>,
    /// Explicit DER coordinator trust anchor (maximum 16 KiB).
    #[arg(long)]
    coordinator_ca: Option<PathBuf>,
    /// Separate DER trust anchor for coordinator-authorized relays.
    #[arg(long, requires = "udp_bind")]
    relay_ca: Option<PathBuf>,
    /// Separate DER trust anchor for tailnet HTTPS services.
    #[arg(long, requires = "udp_bind")]
    service_ca: Option<PathBuf>,
    /// Disable direct UDP, discovery and STUN in the runtime.
    #[arg(long, requires = "udp_bind")]
    relay_only: bool,
    #[arg(long, default_value = "femto-host")]
    hostname: String,
    /// Private regular file containing an auth key (maximum 4096 bytes).
    #[arg(long, conflicts_with = "followup_file")]
    auth_key_file: Option<PathBuf>,
    /// Private file containing the previously returned approval URL.
    #[arg(long)]
    followup_file: Option<PathBuf>,
    /// Require this assigned IPv4 address instead of discovering it from the map.
    #[arg(long)]
    ipv4: Option<Ipv4Addr>,
    /// Finish after registration without starting a map stream.
    #[arg(long)]
    register_only: bool,
    /// Prepare or resume durable node-key rotation against this coordinator.
    #[arg(long)]
    rotate_node_key: bool,
    /// Maximum HTTP/2 events to observe before disconnecting.
    #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u16).range(1..=4096))]
    events: u16,
    /// Run the concurrent control/UDP loop on this numeric bind address.
    #[arg(long, conflicts_with_all = ["register_only", "events"])]
    udp_bind: Option<SocketAddr>,
    /// Maximum UDP runtime duration after registration.
    #[arg(long, default_value_t = 60, requires = "udp_bind", value_parser = clap::value_parser!(u16).range(1..=3600))]
    run_seconds: u16,
    /// Echo authorized tailnet UDP datagrams on this application port.
    #[arg(long, requires = "udp_bind", value_parser = clap::value_parser!(u16).range(1..=65535))]
    udp_echo_port: Option<u16>,
    /// Send one bounded echo probe to this exact authenticated tailnet name.
    #[arg(long, requires = "udp_bind", conflicts_with = "udp_echo_port")]
    udp_probe_peer: Option<String>,
    #[arg(long, default_value_t = 9000, requires = "udp_probe_peer", value_parser = clap::value_parser!(u16).range(1..=65535))]
    udp_probe_port: u16,
    /// Local tailnet port for reply policy; zero allocates an ephemeral port.
    #[arg(long, default_value_t = 0, requires = "udp_probe_peer")]
    udp_probe_local_port: u16,
    /// Fetch an OPDS 2 feed through the enrolled tailnet, then disconnect.
    #[arg(long, requires = "udp_bind", conflicts_with_all = ["udp_probe_peer", "udp_echo_port"], value_parser = catalog_runtime::target)]
    opds_url: Option<String>,
    /// Private JSON Basic credential file scoped to the catalog origin.
    #[arg(long, requires = "opds_url")]
    opds_credentials: Option<PathBuf>,
    /// Permit catalog Basic credentials over HTTP inside the encrypted tailnet.
    #[arg(long, requires = "opds_credentials")]
    opds_allow_http_auth: bool,
    /// Download a selected direct EPUB into this new file after catalog retrieval.
    #[arg(long, requires = "opds_url")]
    opds_output: Option<PathBuf>,
    /// One-based publication number, including grouped publications.
    #[arg(long, default_value_t = 1, requires = "opds_output", value_parser = clap::value_parser!(u32).range(1..=256))]
    opds_publication: u32,
    /// One-based eligible direct EPUB link in the selected publication.
    #[arg(long, default_value_t = 1, requires = "opds_output", value_parser = clap::value_parser!(u32).range(1..=1024))]
    opds_acquisition: u32,
    /// Browse catalogs interactively through the enrolled tailnet.
    #[arg(long, requires = "opds_url", conflicts_with = "opds_output")]
    opds_browse: bool,
    /// Existing directory for downloads selected within the tailnet browser.
    #[arg(long, requires = "opds_browse")]
    opds_download_dir: Option<PathBuf>,
    /// Run a KOReader operation through this enrolled tailnet.
    #[arg(long, requires_all = ["udp_bind", "kosync_action"], conflicts_with_all = ["opds_url", "udp_probe_peer", "udp_echo_port"])]
    kosync_config: Option<PathBuf>,
    #[arg(long, requires = "kosync_config", value_enum)]
    kosync_action: Option<crate::kosync::Operation>,
    #[arg(long, requires_all = ["kosync_config", "kosync_state"])]
    kosync_book: Option<PathBuf>,
    #[arg(long, requires_all = ["kosync_config", "kosync_book"])]
    kosync_state: Option<PathBuf>,
}

fn clock(started: Instant) -> Clock {
    Clock {
        unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .unwrap_or(-1),
        monotonic_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
    }
}

fn private_text(path: &Path) -> Result<String> {
    fn check(metadata: &fs::Metadata) -> Result<()> {
        if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
            bail!("credential input must be a private regular file");
        }
        Ok(())
    }
    check(&fs::symlink_metadata(path)?)?;
    let file = File::open(path)?;
    check(&file.metadata()?)?;
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        bail!("credential input exceeds 4096 bytes");
    }
    let value =
        String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("credential input is not UTF-8"))?;
    let value = value.trim_end_matches(['\r', '\n']);
    if value.is_empty()
        || value
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        bail!("credential input must contain one nonempty token");
    }
    Ok(value.to_owned())
}

pub fn connect(args: ConnectArgs) -> Result<()> {
    let coordinator_tls = crate::host_trust::load_tls(args.coordinator_ca.as_deref())?;
    let transport_config = femto_microlink::host_runtime::TransportConfig {
        relay_tls: crate::host_trust::load_tls(args.relay_ca.as_deref())?,
        direct_udp: !args.relay_only,
    };
    let service_tls = crate::host_trust::load_service_tls(args.service_ca.as_deref())?;
    let control_key = args
        .control_key
        .as_deref()
        .map(PublicKey::<Machine>::parse)
        .transpose()
        .context("invalid public control key")?;
    if args.hostname.is_empty()
        || args.hostname.len() > 63
        || !args
            .hostname
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        bail!("hostname must contain 1 to 63 ASCII letters, digits, or hyphens");
    }
    // Read credentials before creating an identity or opening a network socket.
    let auth_key = args
        .auth_key_file
        .as_deref()
        .map(private_text)
        .transpose()?;
    let followup = args
        .followup_file
        .as_deref()
        .map(private_text)
        .transpose()?;
    if let Some(value) = &followup {
        ApprovalUrl::parse(value).context("invalid follow-up approval URL")?;
    }
    let catalog_auth = args
        .opds_credentials
        .as_deref()
        .map(|path| {
            crate::catalog::credentials(
                path,
                args.opds_url.as_deref().unwrap_or_default(),
                args.opds_allow_http_auth,
            )
        })
        .transpose()?;
    if let Some(directory) = &args.opds_download_dir {
        anyhow::ensure!(directory.is_dir(), "browser download directory must exist");
    }
    let catalog_download = args
        .opds_output
        .as_deref()
        .map(|output| {
            Ok::<_, anyhow::Error>(catalog_runtime::Download {
                destination: crate::catalog::AcquisitionFile::new(output)?,
                publication: args.opds_publication as usize,
                acquisition: args.opds_acquisition as usize,
            })
        })
        .transpose()?;
    let kosync_request = args
        .kosync_config
        .as_deref()
        .map(|path| {
            let settings = crate::kosync::settings(path)?;
            anyhow::ensure!(
                settings.route == femto_kosync::config::Route::Tailnet,
                "KOReader configuration must select the tailnet route"
            );
            catalog_runtime::target(settings.client.server()).map_err(anyhow::Error::msg)?;
            crate::kosync::Prepared::new(
                settings,
                args.kosync_action.context("KOReader action required")?,
                args.kosync_book.as_deref(),
                args.kosync_state.as_deref(),
                Instant::now() + Duration::from_secs(u64::from(args.run_seconds)),
                &AtomicBool::new(false),
            )
        })
        .transpose()?;
    let mut store = HostStore::open(&args.directory)?;
    let host_info = HostInfo {
        ipn_version: format!("femto/{}", env!("CARGO_PKG_VERSION")),
        backend_log_id: String::new(),
        os: std::env::consts::OS.into(),
        os_version: String::new(),
        device_model: "femto-host".into(),
        hostname: args.hostname,
        machine: std::env::consts::ARCH.into(),
        architecture: std::env::consts::ARCH.into(),
        no_logs_no_support: true,
        userspace: true,
        net_info: None,
    };
    let femto_microlink::host_registration::Registration {
        mut control,
        identity,
        outcome,
    } = femto_microlink::host_registration::register_with_tls(
        &mut store,
        femto_microlink::host_registration::Options {
            host: args.host.clone(),
            address: args.endpoint,
            pinned_key: control_key,
            host_info: host_info.clone(),
            auth_key,
            followup,
            rotate_node_key: args.rotate_node_key,
        },
        Arc::new(AtomicBool::new(false)),
        &coordinator_tls,
    )?;
    let node_key = identity.node_key().public();
    match outcome {
        Outcome::Authorized => println!(
            "{}",
            serde_json::json!({"registration":"authorized","node_key":node_key.to_string()})
        ),
        Outcome::AwaitingAuthorization(url) => {
            println!(
                "{}",
                serde_json::json!({"registration":"pending","approval_url":url.as_ref().map(|u| u.as_str())})
            );
            control.close();
            return Ok(());
        }
        Outcome::Expired => bail!(
            "node key expired; retry with --rotate-node-key to prepare or resume reauthorization"
        ),
        Outcome::Rejected => bail!("coordinator rejected registration"),
    }
    if args.register_only {
        control.close();
        return Ok(());
    }
    let started = Instant::now();
    let timestamps = TimestampClock::open(
        store,
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    )?;
    let mut network = Network::with_address(&identity, timestamps, 256, 8, args.ipv4)?;
    let map = MapRequest::new(
        CAPABILITY_VERSION,
        node_key,
        identity.disco_key().public(),
        host_info.clone(),
    )
    .streaming();
    if let Some(bind) = args.udp_bind {
        let udp = UdpTransport::bind(bind).context("bind tailnet UDP socket")?;
        let endpoints = control.endpoint_publisher(identity.machine_key(), &map)?;
        let mut reconnect = reconnect::Controller::with_tls(
            args.host.clone(),
            args.endpoint,
            control_key,
            identity.clone(),
            host_info,
            endpoints,
            coordinator_tls,
        );
        let local_endpoint = match udp.local_addr()? {
            SocketAddr::V4(address)
                if transport_config.direct_udp
                    && !address.ip().is_unspecified()
                    && !address.ip().is_loopback() =>
            {
                Some(address)
            }
            _ => None,
        };
        let mut owner = ControlledNetwork::start(control.into_inner(), map, network)?;
        if let Some(request) = kosync_request {
            return catalog_runtime::run(
                owner,
                udp,
                reconnect,
                identity.node_key().clone(),
                catalog_runtime::Work::Kosync {
                    request: Box::new(request),
                    started,
                },
                transport_config,
                service_tls,
            );
        }

        if let Some(source) = args.opds_url {
            let request = catalog_runtime::Request {
                source,
                auth: catalog_auth,
                download: catalog_download,
                started,
                deadline: started + Duration::from_secs(u64::from(args.run_seconds)),
            };
            let work = if args.opds_browse {
                catalog_runtime::Work::Browse {
                    request,
                    download_dir: args.opds_download_dir,
                }
            } else {
                catalog_runtime::Work::Catalog(request)
            };
            return catalog_runtime::run(
                owner,
                udp,
                reconnect,
                identity.node_key().clone(),
                work,
                transport_config,
                service_tls,
            );
        }
        let mut relays = femto_microlink::host_derp::Pool::default();
        let mut dialer = femto_microlink::host_relay_worker::Dialer::default();
        let (mut relay_events, mut relay_rejected, mut relays_removed) = (0u64, 0u64, 0u64);
        println!(
            "{}",
            serde_json::json!({"udp":"listening","address":udp.local_addr()?})
        );
        let deadline = started + Duration::from_secs(u64::from(args.run_seconds));
        let mut address = None;
        let (mut received, mut rejected, mut sent, mut send_errors) = (0u64, 0u64, 0u64, 0u64);
        let mut probes_queued = 0u64;
        let mut stun_sent = 0u64;
        let mut observed_endpoints = 0usize;
        let mut echo = args
            .udp_echo_port
            .map(femto_microlink::host_network::UdpEcho::new)
            .transpose()?;
        let (mut echo_queued, mut echo_dropped) = (0u64, 0u64);
        let mut probe = if let Some(name) = &args.udp_probe_peer {
            let payload = format!(
                "femto-udp-{}",
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
            )
            .into_bytes();
            let request = femto_microlink::udp_request::Request::new(
                name,
                args.udp_probe_port,
                args.udp_probe_local_port,
                &payload,
                clock(started).monotonic_ms,
                (u64::from(args.run_seconds) * 1000).min(60000),
            )?;
            Some((request, payload))
        } else {
            None
        };
        let probe_requested = probe.is_some();
        while Instant::now() < deadline {
            if !reconnect.poll(&mut owner, &mut relays, &mut dialer, clock(started))? {
                if address.take().is_some() {
                    println!("{}", serde_json::json!({"map":"reconnecting"}));
                }
                echo = args
                    .udp_echo_port
                    .map(femto_microlink::host_network::UdpEcho::new)
                    .transpose()?;
                if probe.is_some() {
                    bail!("control interrupted UDP probe; request was not replayed");
                }
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            let step = (|| -> Result<()> {
                let now = clock(started);
                let progress = owner.run(
                    || now,
                    |network| {
                        let progress = dialer.poll(network, &mut relays, now.monotonic_ms)?;
                        if let Some(plan) = relays.next_plan(network, now.monotonic_ms)? {
                            return Ok((
                                progress,
                                dialer.start_with_tls(
                                    plan,
                                    identity.node_key().clone(),
                                    now.monotonic_ms,
                                    transport_config.relay_tls.clone(),
                                ),
                            ));
                        }
                        Ok((progress, Ok(false)))
                    },
                )?;
                progress.1?;
                if !matches!(
                    progress.0,
                    femto_microlink::host_relay_worker::Progress::Idle
                        | femto_microlink::host_relay_worker::Progress::Pending
                ) {
                    println!(
                        "{}",
                        serde_json::json!({"relay_dial":format!("{:?}", progress.0)})
                    );
                }
                let stats = femto_microlink::host_network::poll_with_optional_udp(
                    &mut owner,
                    transport_config.direct_udp.then_some(&udp),
                    &mut relays,
                    clock(started),
                )?;
                relay_events += stats.relay_events as u64;
                relay_rejected += stats.relay_rejected as u64;
                relays_removed += stats.relays_removed as u64;
                received += stats.received as u64;
                rejected += stats.rejected as u64;
                sent += stats.sent as u64;
                send_errors += stats.send_errors as u64;
                probes_queued += stats.probes_queued as u64;
                stun_sent += stats.stun_sent as u64;
                let now = clock(started);
                let (current, observed, home) = owner.run(
                    || now,
                    |n| {
                        Ok((
                            n.local_address(now.monotonic_ms)?,
                            n.observed_endpoints(now.monotonic_ms)?,
                            relays.home_region(n, now.monotonic_ms),
                        ))
                    },
                )?;
                if current.is_none() && address.is_some() {
                    bail!("local tailnet authorization expired or was withdrawn");
                }
                if current != address {
                    address = current;
                    println!("{}", serde_json::json!({"map":"address","ipv4":address}));
                }
                let publication = if current.is_some() {
                    reconnect.publish(
                        femto_microlink::host_endpoints::Snapshot::new(local_endpoint, &observed)?
                            .with_home_relay(home)?,
                        now.monotonic_ms,
                    )?
                } else {
                    femto_microlink::host_endpoints::Progress::Idle
                };
                if publication == femto_microlink::host_endpoints::Progress::Rejected {
                    owner.close();
                    bail!("coordinator rejected endpoint publication authorization");
                }
                if publication == femto_microlink::host_endpoints::Progress::Cancelled {
                    bail!("endpoint publication cancelled");
                }
                if matches!(
                    publication,
                    femto_microlink::host_endpoints::Progress::Published
                        | femto_microlink::host_endpoints::Progress::Failed
                ) {
                    println!(
                        "{}",
                        serde_json::json!({"endpoints":format!("{publication:?}")})
                    );
                }
                if observed.len() != observed_endpoints {
                    observed_endpoints = observed.len();
                    println!(
                        "{}",
                        serde_json::json!({"stun":"observations","count":observed.len()})
                    );
                }
                if let Some(echo) = &mut echo {
                    let now = clock(started);
                    let stats =
                        owner.run(|| now, |network| echo.poll(network, now.monotonic_ms))?;
                    echo_queued += stats.queued as u64;
                    echo_dropped += stats.dropped as u64;
                }
                if let Some((request, expected)) = &mut probe {
                    let now = clock(started);
                    let response = owner.run(
                        || now,
                        |network| Ok(request.poll(network, now.monotonic_ms)),
                    )??;
                    if let Some(response) = response {
                        if response != *expected {
                            bail!("UDP probe response did not match request");
                        }
                        println!(
                            "{}",
                            serde_json::json!({"udp_probe":"passed","bytes":response.len()})
                        );
                        probe = None;
                    }
                }
                Ok(())
            })();
            if let Err(error) = step
                && owner.stop_reason().is_none()
            {
                return Err(error);
            }
            if probe_requested && probe.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        dialer.cancel();
        relays.close();
        owner.close();
        println!(
            "{}",
            serde_json::json!({"udp":"stopped","received":received,"rejected":rejected,"sent":sent,"send_errors":send_errors,"probes_queued":probes_queued,"stun_sent":stun_sent,"echo_queued":echo_queued,"echo_dropped":echo_dropped,"relay_events":relay_events,"relay_rejected":relay_rejected,"relays_removed":relays_removed})
        );
        if probe.is_some() {
            bail!("UDP probe did not finish before runtime deadline");
        }
        return Ok(());
    }
    let mut stream = control.start_map(&map, &mut network)?;
    for _ in 0..args.events {
        let progress = stream.next(|| clock(started))?;
        match progress {
            Progress::Headers => println!("{}", serde_json::json!({"map":"headers"})),
            Progress::Maps(count) => {
                let address = stream
                    .target()
                    .local_address(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX))?;
                println!(
                    "{}",
                    serde_json::json!({"map":"applied","count":count,"ipv4":address})
                )
            }
            Progress::Ended => {
                println!("{}", serde_json::json!({"map":"ended"}));
                break;
            }
        }
    }
    stream.close();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_arguments_preserve_map_observation_defaults() {
        use clap::Parser;
        #[derive(Parser)]
        struct Command {
            #[command(flatten)]
            connect: ConnectArgs,
        }
        let base = ["tailnet", "/tmp/identity", "--endpoint", "192.0.2.1:443"];
        assert!(Command::try_parse_from(base).is_ok());
        let parse =
            |extra: &[&str]| Command::try_parse_from(base.into_iter().chain(extra.iter().copied()));
        assert!(parse(&["--register-only"]).is_ok());
        assert!(parse(&["--coordinator-ca", "root.der", "--register-only"]).is_ok());
        for flag in ["--relay-ca", "--service-ca"] {
            assert!(parse(&[flag, "root.der"]).is_err());
            assert!(parse(&["--udp-bind", "0.0.0.0:0", flag, "root.der"]).is_ok());
        }
        assert!(parse(&["--relay-only"]).is_err());
        assert!(parse(&["--udp-bind", "0.0.0.0:0", "--relay-only"]).is_ok());
        assert!(
            parse(&["--rotate-node-key", "--register-only"])
                .unwrap()
                .connect
                .rotate_node_key
        );
        let command = parse(&["--udp-bind", "0.0.0.0:0", "--run-seconds", "1"]).unwrap();
        assert_eq!(command.connect.run_seconds, 1);
        assert!(parse(&["--run-seconds", "1"]).is_err());
        assert!(parse(&["--udp-probe-peer", "peer.example"]).is_err());
        assert!(parse(&["--udp-bind", "0.0.0.0:0", "--udp-probe-port", "9000"]).is_err());
        let probe = parse(&[
            "--udp-bind",
            "0.0.0.0:0",
            "--udp-probe-peer",
            "peer.example",
            "--udp-probe-local-port",
            "1234",
        ])
        .unwrap();
        assert_eq!(probe.connect.udp_probe_port, 9000);
        assert_eq!(probe.connect.udp_probe_local_port, 1234);
        assert!(
            parse(&[
                "--udp-bind",
                "0.0.0.0:0",
                "--udp-probe-peer",
                "peer.example",
                "--udp-echo-port",
                "9000"
            ])
            .is_err()
        );
        assert!(parse(&["--udp-echo-port", "9000"]).is_err());
        assert!(parse(&["--udp-bind", "0.0.0.0:0", "--udp-echo-port", "0"]).is_err());
        assert_eq!(
            parse(&["--udp-bind", "0.0.0.0:0", "--udp-echo-port", "9000"])
                .unwrap()
                .connect
                .udp_echo_port,
            Some(9000)
        );
        assert!(parse(&["--udp-bind", "0.0.0.0:0", "--events", "1"]).is_err());
        assert!(parse(&["--udp-bind", "0.0.0.0:0", "--register-only"]).is_err());
        assert!(parse(&["--udp-bind", "0.0.0.0:0", "--run-seconds", "0"]).is_err());
    }
    #[test]
    fn credential_files_are_bounded_private_and_single_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        fs::write(&path, "secret\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(private_text(&path).unwrap(), "secret");
        for body in ["".to_owned(), "secret\nsecond".to_owned(), "x".repeat(4097)] {
            fs::write(&path, body).unwrap();
            assert!(private_text(&path).is_err());
        }
        fs::write(&path, "secret").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(private_text(&path).is_err());
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(private_text(&link).is_err());
        assert!(private_text(dir.path()).is_err());
    }
    #[test]
    fn catalog_arguments_require_runtime_and_validate_before_identity_creation() {
        use clap::Parser;
        #[derive(Parser)]
        struct Command {
            #[command(flatten)]
            connect: ConnectArgs,
        }
        let directory = tempfile::tempdir().unwrap();
        let identity = directory.path().join("identity");
        let identity_text = identity.to_str().unwrap();
        let base = ["tailnet", identity_text, "--endpoint", "192.0.2.1:443"];
        let parse =
            |extra: &[&str]| Command::try_parse_from(base.into_iter().chain(extra.iter().copied()));
        assert!(parse(&["--opds-url", "https://books.example/feed"]).is_err());
        for url in [
            "ftp://books.example/feed",
            "https://100.64.0.2/feed",
            "https://user@books.example/feed",
            "https://books.example/feed#x",
            "https://books.example:0/feed",
        ] {
            assert!(parse(&["--udp-bind", "0.0.0.0:0", "--opds-url", url]).is_err());
        }
        assert!(
            parse(&[
                "--udp-bind",
                "0.0.0.0:0",
                "--opds-url",
                "https://books.example/feed",
                "--udp-echo-port",
                "9000"
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "--udp-bind",
                "0.0.0.0:0",
                "--opds-url",
                "https://books.example/feed",
                "--udp-probe-peer",
                "books.example"
            ])
            .is_err()
        );
        assert!(parse(&["--udp-bind", "0.0.0.0:0", "--opds-credentials", "/missing"]).is_err());
        assert!(
            parse(&[
                "--udp-bind",
                "0.0.0.0:0",
                "--opds-url",
                "http://books.example/feed",
                "--opds-allow-http-auth"
            ])
            .is_err()
        );
        assert!(parse(&["--udp-bind", "0.0.0.0:0", "--opds-output", "book.epub"]).is_err());
        assert!(
            parse(&[
                "--udp-bind",
                "0.0.0.0:0",
                "--opds-url",
                "https://books.example/feed",
                "--opds-publication",
                "2"
            ])
            .is_err()
        );
        assert!(parse(&["--udp-bind", "0.0.0.0:0", "--opds-browse"]).is_err());
        assert!(
            parse(&[
                "--udp-bind",
                "0.0.0.0:0",
                "--opds-url",
                "https://books.example/feed",
                "--opds-download-dir",
                "/tmp"
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "--udp-bind",
                "0.0.0.0:0",
                "--opds-url",
                "https://books.example/feed",
                "--opds-browse",
                "--opds-output",
                "book.epub"
            ])
            .is_err()
        );
        let browse = parse(&[
            "--udp-bind",
            "0.0.0.0:0",
            "--opds-url",
            "https://books.example/feed",
            "--opds-browse",
        ])
        .unwrap();
        assert!(browse.connect.opds_browse);
        let absent = directory.path().join("absent-directory");
        let browse = parse(&[
            "--udp-bind",
            "0.0.0.0:0",
            "--opds-url",
            "https://books.example/feed",
            "--opds-browse",
            "--opds-download-dir",
            absent.to_str().unwrap(),
        ])
        .unwrap();
        assert!(connect(browse.connect).is_err());
        assert!(!identity.exists());
        let download = parse(&[
            "--udp-bind",
            "0.0.0.0:0",
            "--opds-url",
            "https://books.example/feed",
            "--opds-output",
            "book.epub",
            "--opds-publication",
            "2",
            "--opds-acquisition",
            "3",
        ])
        .unwrap();
        assert_eq!(download.connect.opds_publication, 2);
        assert_eq!(download.connect.opds_acquisition, 3);
        let existing = directory.path().join("existing.epub");
        fs::write(&existing, b"preserve").unwrap();
        let command = parse(&[
            "--udp-bind",
            "0.0.0.0:0",
            "--opds-url",
            "https://books.example/feed",
            "--opds-output",
            existing.to_str().unwrap(),
        ])
        .unwrap();
        assert!(connect(command.connect).is_err());
        assert!(!identity.exists());
        assert_eq!(fs::read(&existing).unwrap(), b"preserve");
        let command = parse(&[
            "--udp-bind",
            "0.0.0.0:0",
            "--opds-url",
            "https://BOOKS.example:443/feed?q=1",
            "--run-seconds",
            "10",
        ])
        .unwrap();
        assert_eq!(
            command.connect.opds_url.as_deref(),
            Some("https://books.example/feed?q=1")
        );
        let missing = directory.path().join("missing-credentials");
        let command = parse(&[
            "--udp-bind",
            "0.0.0.0:0",
            "--opds-url",
            "https://books.example/feed",
            "--opds-credentials",
            missing.to_str().unwrap(),
        ])
        .unwrap();
        assert!(connect(command.connect).is_err());
        assert!(!identity.exists());
    }
}
