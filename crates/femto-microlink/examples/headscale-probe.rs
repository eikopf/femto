//! Local-only independent coordinator probe with an explicit fixture trust root.
#[cfg(all(unix, not(target_os = "espidf")))]
#[path = "support/transport.rs"]
mod fixture_transport;
#[cfg(all(unix, not(target_os = "espidf")))]
#[path = "support/path_migration.rs"]
mod path_migration;
#[cfg(all(unix, not(target_os = "espidf")))]
#[path = "support/response_loss.rs"]
mod response_loss;
#[cfg(all(unix, not(target_os = "espidf")))]
use fixture_transport::{FixtureDriver, pump_fixture_relay};
#[cfg(all(unix, not(target_os = "espidf")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use femto_microlink::{
        bootstrap,
        control_session::{ControlSession, Endpoint},
        coordinator::{Clock, Coordinator},
        enrollment::Outcome,
        host_tls::TlsConnector,
        protocol::{
            CAPABILITY_VERSION,
            control::{HostInfo, MapRequest, RegisterRequest},
        },
        relay::Certificate,
        store::HostStore,
    };
    use std::{
        fs,
        net::SocketAddr,
        sync::{Arc, atomic::AtomicBool},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    let args: Vec<_> = std::env::args().collect();
    if !(args.len() == 6
        || (args.len() == 7
            && matches!(
                args[5].as_str(),
                "udp-peer"
                    | "tcp-peer"
                    | "tls-peer"
                    | "derp-peer"
                    | "derp-tcp-peer"
                    | "derp-tls-peer"
            )))
        || !matches!(
            args[5].as_str(),
            "register"
                | "rotate"
                | "expect-expired"
                | "watch-expiry"
                | "watch-peers"
                | "reconnect"
                | "udp-peer"
                | "tcp-peer"
                | "tls-peer"
                | "derp-peer"
                | "derp-tcp-peer"
                | "derp-tls-peer"
        )
    {
        return Err(
            "usage: headscale-probe LOOPBACK:PORT ROOT.der AUTH-FILE IDENTITY-DIR register|rotate|expect-expired|watch-expiry|watch-peers|reconnect|udp-peer|tcp-peer|tls-peer|derp-peer|derp-tcp-peer|derp-tls-peer [PORT]"
                .into(),
        );
    }
    let address: SocketAddr = args[1].parse()?;
    if !address.ip().is_loopback() {
        return Err("probe requires a loopback coordinator".into());
    }
    let timeout = Duration::from_secs(
        if matches!(args[5].as_str(), "tcp-peer" | "derp-tcp-peer") {
            60
        } else if matches!(
            args[5].as_str(),
            "udp-peer" | "tls-peer" | "derp-peer" | "derp-tcp-peer" | "derp-tls-peer"
        ) {
            40
        } else {
            10
        },
    );
    let mut roots = rustls::RootCertStore::empty();
    roots.add(rustls::pki_types::CertificateDer::from(fs::read(&args[2])?))?;
    let connector = TlsConnector::with_roots(roots, timeout)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let host = "localhost";
    let tls = connector.connect(
        address,
        host,
        &Certificate::Name(host.into()),
        cancelled.clone(),
    )?;
    let key = bootstrap::exchange(tls, host, address.port(), CAPABILITY_VERSION)?;
    println!("verified TLS bootstrap");
    let mut store = HostStore::open(&args[4])?;
    let pending = if args[5] == "rotate" {
        Some(store.begin_node_key_rotation(host, address.port())?.clone())
    } else {
        None
    };
    let identity = if let Some(pending) = &pending {
        pending.candidate().clone()
    } else {
        store.identity()?.clone()
    };
    let tls = connector.connect(
        address,
        host,
        &Certificate::Name(host.into()),
        cancelled.clone(),
    )?;
    let endpoint = Endpoint {
        host: host.into(),
        port: address.port(),
        control_key: key,
        version: CAPABILITY_VERSION,
    };
    let mut control =
        ControlSession::upgrade(tls, &endpoint, identity.machine_key(), timeout, cancelled)?;
    println!("verified Noise/HTTP2 upgrade");
    let hostinfo = HostInfo::esp32("femto-independent-probe", "");
    let mut request = RegisterRequest::new(
        CAPABILITY_VERSION,
        identity.node_key().public(),
        identity.network_lock_key().public(),
        hostinfo.clone(),
    );
    if !matches!(
        args[5].as_str(),
        "expect-expired"
            | "watch-expiry"
            | "watch-peers"
            | "reconnect"
            | "udp-peer"
            | "tcp-peer"
            | "tls-peer"
            | "derp-peer"
            | "derp-tcp-peer"
            | "derp-tls-peer"
    ) {
        let auth = zeroize::Zeroizing::new(fs::read_to_string(&args[3])?);
        request = request.with_auth_key(auth.trim());
    }
    let outcome = if let Some(pending) = &pending {
        request = request.rotating_from(pending.old_node_key());
        control.register_rotation(pending, &request, |authorized| {
            store
                .complete_node_key_rotation(host, address.port(), authorized)
                .map_err(std::io::Error::other)
        })?
    } else {
        control.register(&request)?
    };
    if args[5] == "expect-expired" {
        if outcome != Outcome::Expired {
            return Err(format!("expected expired registration, received {outcome:?}").into());
        }
        if control.permits_node(identity.node_key().public()) {
            return Err("expired node retained authorization".into());
        }
        let mut coordinator = Coordinator::discovering(identity.node_key().public(), 64, 4, 0)?;
        let map = MapRequest::new(
            CAPABILITY_VERSION,
            identity.node_key().public(),
            identity.disco_key().public(),
            hostinfo,
        )
        .streaming();
        match control.start_map(&map, &mut coordinator) {
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
            _ => return Err("expired registration did not block map access".into()),
        }
        println!(
            "{}",
            serde_json::json!({"mode":"expect-expired", "expired_registration":true,"map_denied":true})
        );
        return Ok(());
    }
    if outcome != Outcome::Authorized {
        return Err("independent coordinator did not authorize registration".into());
    }
    println!("authorized {}", args[5]);
    let mut coordinator = Coordinator::discovering(identity.node_key().public(), 64, 4, 0)?;
    let request = MapRequest::new(
        CAPABILITY_VERSION,
        identity.node_key().public(),
        identity.disco_key().public(),
        hostinfo,
    )
    .streaming();
    let started = Instant::now();
    let clock = || Clock {
        unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64,
        monotonic_ms: started.elapsed().as_millis() as u64,
    };
    if matches!(
        args[5].as_str(),
        "udp-peer" | "tcp-peer" | "tls-peer" | "derp-peer" | "derp-tcp-peer" | "derp-tls-peer"
    ) {
        use femto_microlink::{
            control_worker::ControlledNetwork, network::Network, socket_network,
            socket_udp::UdpTransport, timestamp::TimestampClock, udp_request::Request,
        };
        let port: u16 = args
            .get(6)
            .ok_or("peer probe needs a destination port")?
            .parse()?;
        let timestamps = TimestampClock::open(
            store,
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        )?;
        let network = Network::new(&identity, timestamps, 64, 4)?;
        let mut owner = ControlledNetwork::start(control, request, network)?;
        if args[5] == "derp-peer" {
            use femto_microlink::host_derp::Pool;
            let mut pool = Pool::default();
            let payload = b"femto independent Tailscale UDP probe";
            let mut probe = Request::new(
                "femto-independent-peer.femto.test",
                port,
                49152,
                payload,
                0,
                30000,
            )?;
            let mut events = 0;
            let mut sent = 0;
            loop {
                let stats = pump_fixture_relay(
                    &mut owner,
                    &mut pool,
                    &connector,
                    address,
                    identity.node_key(),
                    clock,
                )?;
                events += stats.0;
                sent += stats.1;
                let reply = owner.run(clock, |network| {
                    Ok(probe.poll(network, clock().monotonic_ms))
                })??;
                if let Some(reply) = reply {
                    if reply != payload || events == 0 || sent == 0 {
                        return Err("DERP echo evidence missing".into());
                    }
                    pool.close();
                    if !owner.close_and_wait(Duration::from_secs(2)) {
                        return Err("DERP probe worker did not join".into());
                    }
                    println!(
                        "{}",
                        serde_json::json!({"mode":"derp-peer", "independent_peer_udp":true,
                        "wireguard_peer_traffic":true, "derp_traffic":true, "direct_udp_sent":0,
                        "relay_events":events, "relay_packets_queued":sent, "tls_verified":true})
                    );
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let relay_only = matches!(args[5].as_str(), "derp-tcp-peer" | "derp-tls-peer");
        let udp = if relay_only {
            None
        } else {
            Some(UdpTransport::bind("0.0.0.0:0".parse()?)?)
        };
        if matches!(args[5].as_str(), "tls-peer" | "derp-tls-peer") {
            use femto_microlink::{application_tcp::ThreadShared, host_tcp_tls};
            use std::io::{Read, Write};
            let mut relays = femto_microlink::host_derp::Pool::default();
            if relay_only {
                while relays.is_empty() {
                    if started.elapsed() >= Duration::from_secs(25) {
                        return Err("TLS fixture relay setup timed out".into());
                    }
                    pump_fixture_relay(
                        &mut owner,
                        &mut relays,
                        &connector,
                        address,
                        identity.node_key(),
                        clock,
                    )?;
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            let shared = ThreadShared::new(FixtureDriver {
                owner,
                udp,
                relays,
                clock,
                direct_sent: 0,
                relay_events: 0,
            });
            let mut roots = rustls::RootCertStore::empty();
            roots.add(rustls::pki_types::CertificateDer::from(fs::read(&args[2])?))?;
            let connector = host_tcp_tls::Connector::with_roots(roots)?;
            let mut stream = connector.connect(
                shared.clone(),
                "femto-independent-peer.femto.test",
                port,
                started + Duration::from_secs(30),
                Arc::new(AtomicBool::new(false)),
            )?;
            stream.write_all(b"GET /fixture HTTP/1.1\r\nHost: femto-independent-peer.femto.test\r\nConnection: close\r\n\r\n")?;
            stream.flush()?;
            let mut response = Vec::new();
            Read::by_ref(&mut stream)
                .take(70 * 1024)
                .read_to_end(&mut response)?;
            let mut expected =
                b"HTTP/1.1 200 OK\r\nContent-Length: 65536\r\nConnection: close\r\n\r\n".to_vec();
            expected.extend((0..65536).map(|index| (index % 251) as u8));
            if response != expected {
                return Err("independent TLS HTTP response mismatch".into());
            }
            drop(stream);
            let (direct_sent, relay_events) =
                shared.with(|driver| Ok((driver.direct_sent, driver.relay_events)))?;
            if relay_only && (direct_sent != 0 || relay_events == 0) {
                return Err("TLS fixture did not use relay-only transport".into());
            }
            if !shared.with(|driver| Ok(driver.owner.close_and_wait(Duration::from_secs(2))))? {
                return Err("TLS probe control worker did not join".into());
            }
            println!(
                "{}",
                serde_json::json!({"mode":args[5], "independent_peer_tls":true,
                "derp_traffic":relay_only, "direct_udp_sent":direct_sent, "relay_events":relay_events,
                "http_body_bytes":65536, "tls_verified":true, "tls_close_notify":true})
            );
            return Ok(());
        }
        if matches!(args[5].as_str(), "tcp-peer" | "derp-tcp-peer") {
            use femto_microlink::{network, tcp};
            use smoltcp::socket::tcp::State;
            let payload: Vec<u8> = (0..65536).map(|index| (index % 251) as u8).collect();
            let mut socket: Option<tcp::Socket> = None;
            let mut written = 0;
            let mut read = 0;
            let mut eof = false;
            let mut received = 0;
            let mut sent = 0;
            let mut handshake_responses = 0;
            let mut established_at = None;
            let mut relays = femto_microlink::host_derp::Pool::default();
            let mut dropped_initiation_at = None;
            let mut dropped_response_at = None;
            let mut relay_response_loss = response_loss::RelayResponseLoss::default();
            let mut retry_recovery_ms = None;
            let mut reported_written = 0;
            let mut dropped_data = Vec::new();
            let mut dropped_other = 0usize;
            let mut migration = (std::env::var("FEMTO_HEADSCALE_MIGRATION_ONLY").as_deref()
                == Ok("1")
                && !relay_only)
                .then(path_migration::Migration::default);
            loop {
                let now = clock();
                if now.monotonic_ms >= 330000 {
                    return Err("independent TCP probe deadline elapsed".into());
                }
                if dropped_initiation_at.is_none() {
                    owner.run(
                        || now,
                        |network| {
                            let now = now.monotonic_ms;
                            if let Some((peer, _)) =
                                network.resolve("femto-independent-peer.femto.test", now)?
                            {
                                if network.ensure_session(peer, now)? {
                                    return Err(std::io::Error::other(
                                        "loss fixture unexpectedly had a session",
                                    )
                                    .into());
                                }
                                for _ in 0..16 {
                                    network.flush_one(now, |_, _, bytes| {
                                        if bytes.len() == 148
                                            && bytes.starts_with(&1u32.to_le_bytes())
                                        {
                                            dropped_initiation_at = Some(now);
                                            Ok(())
                                        } else {
                                            Err(std::io::ErrorKind::WouldBlock.into())
                                        }
                                    })?;
                                    if dropped_initiation_at.is_some() {
                                        break;
                                    }
                                }
                                if dropped_initiation_at.is_none() {
                                    return Err(std::io::Error::other(
                                        "loss fixture could not intercept initiation",
                                    )
                                    .into());
                                }
                            }
                            Ok(())
                        },
                    )?;
                }
                if dropped_data.len() < 3 && written >= 49152 {
                    owner.run(
                        || now,
                        |network| {
                            let installed = network.completed_handshakes();
                            let installed = installed.initiated.saturating_add(installed.responded);
                            if installed >= 2 {
                                // Drain the entire bounded outgoing queue before the ordinary
                                // transport pump. TCP output is generated later in this loop.
                                // Continue across polls until three payload packets are lost.
                                for _ in 0..16 {
                                    if dropped_data.len() == 3 {
                                        break;
                                    }
                                    if !network.flush_one(now.monotonic_ms, |_, _, bytes| {
                                        if bytes.len() > 256
                                            && bytes.starts_with(&4u32.to_le_bytes())
                                        {
                                            dropped_data.push((
                                                now.monotonic_ms,
                                                bytes.len(),
                                                written,
                                                read,
                                                installed,
                                            ));
                                        } else {
                                            dropped_other += 1;
                                        }
                                        Ok(())
                                    })? {
                                        break;
                                    }
                                }
                            }
                            Ok(())
                        },
                    )?;
                }
                if relay_only {
                    let (events, queued, responses) = relay_response_loss.pump(
                        &mut owner,
                        &mut relays,
                        &connector,
                        address,
                        identity.node_key(),
                        clock,
                    )?;
                    dropped_response_at = relay_response_loss.dropped_at;
                    received += events;
                    sent += queued;
                    handshake_responses += responses;
                } else {
                    let stats = if dropped_response_at.is_none() {
                        response_loss::pump_drop_first_response(
                            &mut owner,
                            udp.as_ref().unwrap(),
                            now,
                            &mut dropped_response_at,
                        )?
                    } else if let Some(migration) = &mut migration {
                        migration.pump(
                            path_migration::Pump {
                                owner: &mut owner,
                                pool: &mut relays,
                                udp: udp.as_ref().unwrap(),
                                connector: &connector,
                                address,
                                key: identity.node_key(),
                                clock,
                            },
                            established_at.map(|at| now.monotonic_ms - at),
                            read,
                        )?
                    } else {
                        socket_network::poll_udp(&mut owner, udp.as_ref().unwrap(), now)?
                    };
                    received += stats.received;
                    handshake_responses += stats.handshake_responses;
                    sent += stats.sent;
                }
                let now = clock();
                if retry_recovery_ms.is_none() {
                    let completed =
                        owner.run(|| now, |network| Ok(network.completed_handshakes()))?;
                    if completed.initiated == 0 {
                        if let Some(dropped) = dropped_initiation_at
                            && now.monotonic_ms.saturating_sub(dropped) >= 90_000
                        {
                            return Err(format!("timer-only handshake recovery timed out: dropped_at={dropped}, now={}, received={received}, sent={sent}, responses={handshake_responses}, completed={completed:?}", now.monotonic_ms).into());
                        }
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    let elapsed =
                        now.monotonic_ms - dropped_initiation_at.ok_or("loss injection missing")?;
                    if dropped_response_at.is_none() {
                        return Err("independent response loss injection missing".into());
                    }
                    if elapsed < 10000 {
                        return Err("handshake recovered before automatic retry deadline".into());
                    }
                    retry_recovery_ms = Some(elapsed);
                    eprintln!(
                        "TCP preflight recovered: now={}, initiation_drop={:?}, response_drop={:?}, recovery_ms={elapsed}",
                        now.monotonic_ms, dropped_initiation_at, dropped_response_at
                    );
                }
                let complete = owner.run(
                    || now,
                    |network| {
                        let now = now.monotonic_ms;
                        if socket.is_none() {
                            if let Some((peer, address)) =
                                network.resolve("femto-independent-peer.femto.test", now)?
                            {
                                socket = Some(network.connect_tcp(
                                    peer,
                                    std::net::SocketAddrV4::new(address, port),
                                    now,
                                )?);
                            } else {
                                return Ok(false);
                            }
                        }
                        let socket = socket.as_ref().unwrap();
                        network.poll_tcp(socket, now)?;
                        let state = network.tcp_state(socket, now)?;
                        if eof {
                            return Ok(state == State::Closed);
                        }
                        if !matches!(state, State::Established | State::CloseWait) {
                            return Ok(false);
                        }
                        let established = *established_at.get_or_insert(now);
                        let available =
                            (4096 * (1 + (now - established) / 13000) as usize).min(payload.len());
                        if written < available {
                            match network.write_tcp_io(socket, &payload[written..available], now) {
                                Ok(count) => written += count,
                                Err(network::Error::Io(error))
                                    if error.kind() == std::io::ErrorKind::WouldBlock => {}
                                Err(error) => return Err(error),
                            }
                        }
                        let mut buffer = [0; 4096];
                        match network.read_tcp_io(socket, &mut buffer, now) {
                            Ok(0) => {
                                if written != payload.len() || read != payload.len() {
                                    return Err(std::io::Error::other("premature TCP EOF").into());
                                }
                                eof = true;
                                network.close_tcp(socket, now)?;
                            }
                            Ok(count) => {
                                if payload.get(read..read + count) != Some(&buffer[..count]) {
                                    return Err(std::io::Error::other(
                                        "independent TCP echo mismatch",
                                    )
                                    .into());
                                }
                                read += count;
                            }
                            Err(network::Error::Io(error))
                                if error.kind() == std::io::ErrorKind::WouldBlock => {}
                            Err(error) => return Err(error),
                        }
                        Ok(false)
                    },
                )?;
                if written != reported_written {
                    let completed =
                        owner.run(clock, |network| Ok(network.completed_handshakes()))?;
                    eprintln!(
                        "TCP transfer: now={}, written={written}, read={read}, received={received}, sent={sent}, sessions={completed:?}",
                        now.monotonic_ms
                    );
                    reported_written = written;
                }
                if complete {
                    let elapsed =
                        now.monotonic_ms - established_at.ok_or("TCP never established")?;
                    let completed =
                        owner.run(clock, |network| Ok(network.completed_handshakes()))?;
                    let installed = completed.initiated.saturating_add(completed.responded);
                    if elapsed < 195000 || installed < 2 {
                        return Err(format!("independent TCP renewal evidence missing: elapsed={elapsed}, responses={handshake_responses}, completed={completed:?}").into());
                    }
                    let (drop_at, drop_bytes, written_at_drop, read_at_drop, sessions_at_drop) =
                        dropped_data
                            .first()
                            .copied()
                            .ok_or("independent transport-data loss injection missing")?;
                    let migration_report =
                        migration.as_ref().map(|m| m.report(read)).transpose()?;
                    relays.close();
                    if received == 0 || sent == 0 || !owner.close_and_wait(Duration::from_secs(2)) {
                        return Err("TCP transport evidence or joined shutdown missing".into());
                    }
                    println!(
                        "{}",
                        serde_json::json!({"mode":args[5], "independent_peer_tcp":true,
                        "derp_traffic":relay_only || migration.is_some(), "direct_udp_sent":if relay_only {0} else {migration.as_ref().map_or(sent, |m| m.direct_sent())},
                        "bytes_written":written, "bytes_read":read, "remote_eof":eof,
                        "tcp_closed":true, "renewal_elapsed_ms":elapsed,
                        "authenticated_handshake_responses":handshake_responses,
                        "authenticated_session_installations":installed,
                        "initiator_installations":completed.initiated,
                        "responder_installations":completed.responded,
                        "deliberately_dropped_initiations":1,
                        "deliberately_dropped_responses":usize::from(dropped_response_at.is_some()),
                        "response_drop_at_ms":dropped_response_at,
                        "deliberately_dropped_data_packets":dropped_data.len(),
                        "additional_packets_dropped_during_data_loss":dropped_other,
                        "data_drop_events":dropped_data.iter().map(|(at, bytes, written, read, installed)| serde_json::json!({
                            "at_ms":at, "ciphertext_bytes":bytes, "written_bytes":written,
                            "read_bytes":read, "installed_sessions":installed,
                        })).collect::<Vec<_>>(),
                        "data_drop_at_ms":drop_at,
                        "data_drop_ciphertext_bytes":drop_bytes,
                        "data_drop_written_bytes":written_at_drop,
                        "data_drop_read_bytes":read_at_drop,
                        "data_drop_installed_sessions":sessions_at_drop,
                        "timer_only_handshake_recovery":true,
                        "handshake_retry_recovery_ms":retry_recovery_ms,
                        "transport_received":received, "transport_sent":sent,
                        "path_migration":migration_report})
                    );
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let payload = b"femto independent Tailscale UDP probe";
        let mut probe = Request::new(
            "femto-independent-peer.femto.test",
            port,
            49152,
            payload,
            0,
            30000,
        )?;
        let mut received = 0;
        let mut sent = 0;
        loop {
            let now = clock();
            let stats = socket_network::poll_udp(&mut owner, udp.as_ref().unwrap(), now)?;
            received += stats.received;
            sent += stats.sent;
            let reply =
                owner.run(|| now, |network| Ok(probe.poll(network, now.monotonic_ms)))??;
            if let Some(reply) = reply {
                if reply != payload || received == 0 || sent == 0 {
                    return Err(
                        "independent peer echo mismatch or missing UDP transport evidence".into(),
                    );
                }
                if !owner.close_and_wait(Duration::from_secs(2)) {
                    return Err("independent peer control worker did not join".into());
                }
                println!(
                    "{}",
                    serde_json::json!({
                        "mode":"udp-peer", "independent_peer_udp":true,
                        "wireguard_peer_traffic":true, "transport_received":received,
                        "transport_sent":sent, "derp_traffic":false
                    })
                );
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    if args[5] == "reconnect" {
        use femto_microlink::{
            control_worker::ControlledNetwork,
            endpoints::{Progress as PublicationProgress, Snapshot},
            host_reconnect::Controller,
            network::Network,
            timestamp::TimestampClock,
        };
        use std::{
            io::Write,
            sync::atomic::{AtomicUsize, Ordering},
        };
        let timestamps = TimestampClock::open(
            store,
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        )?;
        let network = Network::new(&identity, timestamps, 64, 4)?;
        let record_path = std::path::Path::new(&args[4]).join("identity.bin");
        let record = zeroize::Zeroizing::new(fs::read(&record_path)?);
        let root = fs::read(&args[2])?;
        let publisher = fixture_publisher(
            &control,
            &request,
            &identity,
            endpoint,
            root.clone(),
            address,
            timeout,
        )?;
        let mut owner = ControlledNetwork::start(control, request, network)?;
        let registrations = Arc::new(AtomicUsize::new(0));
        let count = registrations.clone();
        let setup_identity = identity.clone();
        let mut controller = Controller::with_setup(
            move |cancel| {
                let mut roots = rustls::RootCertStore::empty();
                roots
                    .add(rustls::pki_types::CertificateDer::from(root.clone()))
                    .map_err(std::io::Error::other)?;
                let connector = TlsConnector::with_roots(roots, timeout)?;
                let tls = connector.connect(
                    address,
                    host,
                    &Certificate::Name(host.into()),
                    cancel.clone(),
                )?;
                let key = bootstrap::exchange(tls, host, address.port(), CAPABILITY_VERSION)?;
                let tls = connector.connect(
                    address,
                    host,
                    &Certificate::Name(host.into()),
                    cancel.clone(),
                )?;
                let endpoint = Endpoint {
                    host: host.into(),
                    port: address.port(),
                    control_key: key,
                    version: CAPABILITY_VERSION,
                };
                let mut control = ControlSession::upgrade(
                    tls,
                    &endpoint,
                    setup_identity.machine_key(),
                    timeout,
                    cancel,
                )?;
                let hostinfo = HostInfo::esp32("femto-independent-probe", "");
                let registration = RegisterRequest::new(
                    CAPABILITY_VERSION,
                    setup_identity.node_key().public(),
                    setup_identity.network_lock_key().public(),
                    hostinfo.clone(),
                );
                if control.register(&registration)? != Outcome::Authorized {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "reconnection not authorized",
                    ));
                }
                count.fetch_add(1, Ordering::SeqCst);
                let request = MapRequest::new(
                    CAPABILITY_VERSION,
                    setup_identity.node_key().public(),
                    setup_identity.disco_key().public(),
                    hostinfo,
                )
                .streaming();
                let publisher = fixture_publisher(
                    &control,
                    &request,
                    &setup_identity,
                    endpoint,
                    root.clone(),
                    address,
                    timeout,
                )?;
                Ok((control, request, publisher))
            },
            publisher,
        );
        let mut pool = femto_microlink::host_derp::Pool::default();
        let mut dialer = femto_microlink::host_relay_worker::Dialer::default();
        let mut original = None;
        let mut disconnected = false;
        while started.elapsed() < Duration::from_secs(45) {
            if controller.poll(&mut owner, &mut pool, &mut dialer, clock())? {
                let assigned =
                    match owner.run(clock, |network| network.local_address(clock().monotonic_ms)) {
                        Ok(address) => address,
                        Err(_) if owner.stop_reason().is_some() => continue,
                        Err(error) => return Err(error.into()),
                    };
                if let Some(assigned) = assigned {
                    let hint = if disconnected {
                        "192.0.2.2:41641"
                    } else {
                        "192.0.2.1:41641"
                    };
                    let progress = controller.publish(
                        Snapshot::new(Some(hint.parse()?), &[])?,
                        clock().monotonic_ms,
                    )?;
                    match progress {
                        PublicationProgress::Published => {
                            if disconnected {
                                if original != Some(assigned)
                                    || registrations.load(Ordering::SeqCst) == 0
                                {
                                    return Err(
                                        "reconnect failed authorization continuity checks".into()
                                    );
                                }
                                if fs::read(&record_path)? != *record {
                                    return Err(
                                        "reconnect rewrote identity or timestamp record".into()
                                    );
                                }
                                if !owner.close_and_wait(Duration::from_secs(2)) {
                                    return Err("reconnected worker did not stop".into());
                                }
                                println!(
                                    "{}",
                                    serde_json::json!({"mode":"reconnect", "coordinator_reconnect":true, "fresh_registration":true, "fresh_map_authorization":true, "disconnected_access_denied":true, "identity_record_retained":true, "endpoint_publication_before_restart":true, "endpoint_publication_after_restart":true, "assigned_ipv4":assigned.to_string(), "node_key":identity.node_key().public().to_string()})
                                );
                                return Ok(());
                            } else if original.is_none() {
                                original = Some(assigned);
                                println!("map ready for reconnect");
                                std::io::stdout().flush()?;
                            }
                        }
                        PublicationProgress::Rejected => {
                            return Err("endpoint publication rejected".into());
                        }
                        PublicationProgress::Cancelled if owner.stop_reason().is_none() => {
                            return Err("endpoint publication cancelled independently".into());
                        }
                        _ => {}
                    }
                }
            } else if original.is_some() && !disconnected {
                if owner
                    .run::<()>(clock, |_| {
                        panic!("disconnected control released network access")
                    })
                    .is_ok()
                {
                    return Err("disconnected access was permitted".into());
                }
                disconnected = true;
                println!("control disconnected before reconnect");
                std::io::stdout().flush()?;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        return Err("coordinator reconnection timed out".into());
    }
    if matches!(args[5].as_str(), "watch-expiry" | "watch-peers") {
        use femto_microlink::map_session::MapTarget;
        use femto_microlink::{
            plane::PlaneError,
            protocol::wireguard::{HandshakeInitiator, HandshakeResponder},
            transport::Session,
        };
        use std::io::Write;
        struct Observed {
            coordinator: Coordinator,
            local_key: String,
            expired_node: bool,
            peer: Option<(serde_json::Value, String)>,
            removed: bool,
            peer_ip: Option<std::net::Ipv4Addr>,
            policies: usize,
        }
        impl MapTarget for Observed {
            type Error = femto_microlink::coordinator::MapError;
            fn apply(&mut self, bytes: &[u8], clock: Clock) -> Result<(), Self::Error> {
                let value: serde_json::Value = serde_json::from_slice(bytes).unwrap_or_default();
                let result = self.coordinator.apply(bytes, clock);
                if result.is_ok() {
                    if value.get("PacketFilter").is_some() || value.get("PacketFilters").is_some() {
                        self.policies += 1;
                    }
                    if let Some(peer) = value["PeersChanged"]
                        .as_array()
                        .and_then(|peers| peers.first())
                        && let Some(key) = peer["Key"].as_str()
                    {
                        self.peer = Some((peer["ID"].clone(), key.to_owned()));
                        self.peer_ip = peer["Addresses"].as_array().and_then(|addresses| {
                            addresses
                                .iter()
                                .filter_map(|address| {
                                    address.as_str()?.split('/').next()?.parse().ok()
                                })
                                .next()
                        });
                    }
                    if let Some((id, _)) = &self.peer {
                        self.removed |= value["PeersRemoved"]
                            .as_array()
                            .is_some_and(|ids| ids.contains(id));
                    }
                }
                self.expired_node |= value["Node"]["Expired"] == true
                    && value["Node"]["Key"].as_str() == Some(self.local_key.as_str())
                    && matches!(result, Err(Self::Error::Node(_)));
                result
            }
            fn disconnect(&mut self) {
                self.coordinator.disconnect();
            }
        }
        let mut observed = Observed {
            coordinator,
            local_key: identity.node_key().public().to_string(),
            expired_node: false,
            peer: None,
            removed: false,
            peer_ip: None,
            policies: 0,
        };
        let mut stream = control.start_map(&request, &mut observed)?;
        let mut ready = false;
        let mut added = false;
        let mut packet_check = None;
        let mut policy_verified = false;
        for _ in 0..32 {
            let result = stream.next(clock);
            let target = stream.target();
            let authorized = target
                .coordinator
                .local_address(clock().monotonic_ms)?
                .is_some();
            if ready && target.expired_node && !authorized {
                println!(
                    "{}",
                    serde_json::json!({"mode":"watch-expiry", "authenticated_expiry":true, "map_authorization_revoked":true})
                );
                return Ok(());
            }
            result?;
            if args[5] == "watch-peers"
                && ready
                && authorized
                && let Some((_, key)) = &target.peer
            {
                let permitted = target
                    .coordinator
                    .route(
                        femto_microlink::protocol::key::PublicKey::parse(key)?,
                        clock().monotonic_ms,
                    )?
                    .is_some();
                if !added && permitted {
                    let peer_path = std::path::Path::new(&args[4])
                        .parent()
                        .ok_or("identity parent missing")?
                        .join("peer-identity");
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let peer_store = loop {
                        match HostStore::open(&peer_path) {
                            Ok(store) => break store,
                            Err(femto_microlink::store::StoreError::Locked)
                                if Instant::now() < deadline =>
                            {
                                std::thread::sleep(Duration::from_millis(10))
                            }
                            Err(error) => return Err(error.into()),
                        }
                    };
                    let remote = peer_store.identity()?;
                    if remote.node_key().public().to_string() != *key {
                        return Err("fixture peer identity does not match authenticated map".into());
                    }
                    let mut initiator = HandshakeInitiator::new(
                        remote.node_key(),
                        identity.node_key().public(),
                        1,
                    )?;
                    let initiation = initiator.initiation(SystemTime::now())?;
                    let response =
                        HandshakeResponder::respond(identity.node_key(), &initiation, 2)?;
                    let now = clock().monotonic_ms;
                    let mut sender = Session::new(initiator.finish(&response.packet)?, now);
                    let plane = target.coordinator.data_plane();
                    let permit = plane.begin_handshake(remote.node_key().public(), now)?;
                    plane.install(permit, Session::new(response.session, now), now)?;
                    let mut udp = vec![0; 28];
                    udp[0] = 0x45;
                    udp[3] = 28;
                    udp[8] = 64;
                    udp[9] = 17;
                    udp[12..16]
                        .copy_from_slice(&target.peer_ip.ok_or("peer IPv4 missing")?.octets());
                    udp[16..20].copy_from_slice(
                        &target
                            .coordinator
                            .local_address(now)?
                            .ok_or("local IPv4 missing")?
                            .octets(),
                    );
                    udp[20..22].copy_from_slice(&9000_u16.to_be_bytes());
                    udp[22..24].copy_from_slice(&1234_u16.to_be_bytes());
                    udp[25] = 8;
                    let plane = target.coordinator.data_plane();
                    let encrypted = sender.encrypt(&udp, now)?;
                    let packet = plane
                        .receive(remote.node_key().public(), &encrypted, now)?
                        .ok_or("packet missing")?;
                    if plane.release(packet, now)? != udp {
                        return Err("packet changed".into());
                    }
                    let encrypted = sender.encrypt(&udp, now)?;
                    let queued = plane
                        .receive(remote.node_key().public(), &encrypted, now)?
                        .ok_or("queued packet missing")?;
                    packet_check = Some((sender, queued, udp, target.policies));
                    added = true;
                    println!("peer authorized {key}");
                    println!("packet authorized before policy update");
                    std::io::stdout().flush()?;
                }
                if packet_check
                    .as_ref()
                    .is_some_and(|(_, _, _, policies)| target.policies > *policies)
                {
                    let (mut sender, queued, udp, _) = packet_check.take().unwrap();
                    if !permitted {
                        return Err(
                            "policy update removed peer instead of narrowing packet access".into(),
                        );
                    }
                    let now = clock().monotonic_ms;
                    let plane = target.coordinator.data_plane();
                    if plane.release(queued, now).is_ok() {
                        return Err("policy update released revoked queued packet".into());
                    }
                    let encrypted = sender.encrypt(&udp, now)?;
                    if !matches!(
                        plane.receive(
                            femto_microlink::protocol::key::PublicKey::parse(key)?,
                            &encrypted,
                            now
                        ),
                        Err(PlaneError::Unauthorized)
                    ) {
                        return Err("policy update did not deny new packet".into());
                    }
                    let mut allowed = udp;
                    allowed[22..24].copy_from_slice(&1235_u16.to_be_bytes());
                    let encrypted = sender.encrypt(&allowed, now)?;
                    let packet = plane
                        .receive(
                            femto_microlink::protocol::key::PublicKey::parse(key)?,
                            &encrypted,
                            now,
                        )?
                        .ok_or("allowed packet missing after update")?;
                    if plane.release(packet, now)? != allowed {
                        return Err("allowed packet changed after update".into());
                    }
                    policy_verified = true;
                    println!("packet revoked by policy update");
                    std::io::stdout().flush()?;
                }
                if added && policy_verified && target.removed && !permitted {
                    println!(
                        "{}",
                        serde_json::json!({"mode":"watch-peers", "incremental_peer_added":true,"incremental_peer_removed":true, "local_authorization_retained":true, "policy_revocation":true,"policy_retained_port":true})
                    );
                    return Ok(());
                }
            }
            if !ready && authorized {
                ready = true;
                println!(
                    "map ready for {}",
                    if args[5] == "watch-peers" {
                        "peers"
                    } else {
                        "expiry"
                    }
                );
                std::io::stdout().flush()?;
            }
        }
        return Err("active map did not complete the requested update sequence".into());
    }
    let mut stream = control.start_map(&request, &mut coordinator)?;
    for _ in 0..8 {
        stream.next(clock)?;
        if let Some(address) = stream.coordinator().local_address(clock().monotonic_ms)? {
            println!(
                "{}",
                serde_json::json!({"mode":args[5], "node_key":identity.node_key().public().to_string(),"assigned_ipv4":address.to_string(),"capability_version":CAPABILITY_VERSION})
            );
            return Ok(());
        }
    }
    Err("map did not authorize a local IPv4 address".into())
}
#[cfg(not(all(unix, not(target_os = "espidf"))))]
fn main() {
    eprintln!("This probe requires Unix host TLS support.");
}

#[cfg(all(unix, not(target_os = "espidf")))]
fn fixture_publisher(
    control: &femto_microlink::control_session::ControlSession<
        femto_microlink::host_tls::TlsStream,
    >,
    request: &femto_microlink::protocol::control::MapRequest,
    identity: &femto_microlink::protocol::identity::DeviceIdentity,
    endpoint: femto_microlink::control_session::Endpoint,
    root: Vec<u8>,
    address: std::net::SocketAddr,
    timeout: std::time::Duration,
) -> std::io::Result<femto_microlink::endpoints::Publisher> {
    let machine = identity.machine_key().clone();
    control.endpoint_publisher(request, 2 * 1024 * 1024, move |cancel| {
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(root.clone()))
            .map_err(std::io::Error::other)?;
        let connector = femto_microlink::host_tls::TlsConnector::with_roots(roots, timeout)?;
        let tls = connector.connect(
            address,
            &endpoint.host,
            &femto_microlink::relay::Certificate::Name(endpoint.host.clone()),
            cancel.clone(),
        )?;
        femto_microlink::control_session::ControlSession::upgrade(
            tls, &endpoint, &machine, timeout, cancel,
        )
    })
}
