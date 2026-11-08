//! Opt-in public-relay diagnostic using two ephemeral, locally owned identities.
#[cfg(all(unix, not(target_os = "espidf")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use femto_microlink::{
        derp::RelaySession,
        handshake::Handshakes,
        host_tls::{TlsConnector, TlsStream},
        plane::{DataPlane, Peer},
        protocol::{
            derp::Event,
            identity::DeviceIdentity,
            key::{Node, PublicKey},
        },
        relay::{Address, Certificate, RelayNode},
        store::HostStore,
        timestamp::TimestampClock,
    };
    use std::{
        net::{Ipv4Addr, SocketAddr},
        sync::{Arc, atomic::AtomicBool},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    type Error = Box<dyn std::error::Error>;
    fn plane(local: Ipv4Addr, remote: Ipv4Addr, key: PublicKey<Node>) -> Result<DataPlane, Error> {
        let mut plane = DataPlane::new(local, 1, 1, 0)?;
        let policy = serde_json::to_vec(
            &serde_json::json!({"PacketFilter":[{"SrcIPs":[remote.to_string()],"DstPorts":[{"IP":local.to_string(),"Ports":{"First":1234,"Last":1234}}],"IPProto":[17]}]}),
        )?;
        plane.apply_update(
            true,
            &[Peer {
                key,
                addresses: vec![remote],
                expires_ms: 120_000,
            }],
            &[],
            &policy,
            0,
        )?;
        Ok(plane)
    }
    fn udp(source: Ipv4Addr, destination: Ipv4Addr, payload: &[u8]) -> Vec<u8> {
        let len = 28 + payload.len();
        let mut bytes = vec![0; len];
        bytes[0] = 0x45;
        bytes[2..4].copy_from_slice(&(len as u16).to_be_bytes());
        bytes[8] = 64;
        bytes[9] = 17;
        bytes[12..16].copy_from_slice(&source.octets());
        bytes[16..20].copy_from_slice(&destination.octets());
        let mut checksum: u32 = bytes[..20]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u32::from(u16::from_be_bytes([b[0], b[1]])))
            .sum();
        while checksum > 0xffff {
            checksum = (checksum & 0xffff) + (checksum >> 16);
        }
        bytes[10..12].copy_from_slice(&(!(checksum as u16)).to_be_bytes());
        bytes[20..22].copy_from_slice(&9000_u16.to_be_bytes());
        bytes[22..24].copy_from_slice(&1234_u16.to_be_bytes());
        bytes[24..26].copy_from_slice(&((len - 20) as u16).to_be_bytes());
        bytes[28..].copy_from_slice(payload);
        bytes
    }
    fn receive_packet(
        relay: &mut RelaySession<TlsStream>,
        key: PublicKey<Node>,
    ) -> Result<Vec<u8>, Error> {
        for _ in 0..8 {
            match relay.receive()? {
                Event::Packet { source, payload } if source == key => return Ok(payload),
                Event::KeepAlive | Event::Ping(_) | Event::Pong(_) => (),
                _ => return Err("unexpected relay source or control event".into()),
            }
        }
        Err("relay control-event limit exceeded".into())
    }
    fn receive(
        relay: &mut RelaySession<TlsStream>,
        plane: &mut DataPlane,
        handshakes: &mut Handshakes,
        key: PublicKey<Node>,
        expected: &[u8],
        started: Instant,
    ) -> Result<(), Error> {
        let payload = receive_packet(relay, key)?;
        let now = started.elapsed().as_millis() as u64;
        let (authenticated_peer, pending) = handshakes.receive(plane, &payload, now)?;
        if authenticated_peer != key {
            return Err("authenticated peer mismatch".into());
        }
        let pending = pending.ok_or("unexpected keepalive")?;
        if plane.release(pending, now)? != expected {
            return Err("packet content mismatch".into());
        }
        Ok(())
    }
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("usage: derp-probe HOSTNAME NUMERIC-IP:PORT".into());
    }
    let address: SocketAddr = args[2].parse()?;
    let started = Instant::now();
    let temporary = tempfile::tempdir()?;
    let store = HostStore::open(temporary.path().join("identity"))?;
    let a = store.identity()?.clone();
    let mut timestamps = TimestampClock::open(
        store,
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    )?;
    let b = DeviceIdentity::generate()?;
    let ak = a.node_key().public();
    let bk = b.node_key().public();
    let mut ah = Handshakes::new(a.node_key().clone(), 2, 0)?;
    let mut bh = Handshakes::new(b.node_key().clone(), 2, 0)?;
    let aip = Ipv4Addr::new(100, 64, 0, 1);
    let bip = Ipv4Addr::new(100, 64, 0, 2);
    let mut ap = plane(aip, bip, bk)?;
    let mut bp = plane(bip, aip, ak)?;
    let node = RelayNode {
        name: "probe".into(),
        host_name: args[1].clone(),
        certificate: Certificate::Name(args[1].clone()),
        ipv4: Address::Dns,
        ipv6: Address::Dns,
        tls_port: address.port(),
        stun_port: None,
        stun_only: false,
    };
    let connector = TlsConnector::mozilla(Duration::from_secs(5))?;
    let cancelled = Arc::new(AtomicBool::new(false));
    eprintln!("Connecting two ephemeral relay clients");
    let mut ar = connector.connect_derp(&node, address, a.node_key().clone(), cancelled.clone())?;
    let mut br = connector.connect_derp(&node, address, b.node_key().clone(), cancelled)?;
    for round in 0..2 {
        let initiation = ah.begin(
            &mut ap,
            bk,
            &mut timestamps,
            started.elapsed().as_millis() as u64,
        )?;
        ar.send(bk, &initiation)?;
        let received = receive_packet(&mut br, ak)?;
        let (peer, response) =
            bh.respond(&mut bp, &received, started.elapsed().as_millis() as u64)?;
        if peer != ak {
            return Err("WireGuard handshake peer mismatch".into());
        }
        br.send(ak, &response)?;
        let received = receive_packet(&mut ar, bk)?;
        if ah.finish(&mut ap, &received, started.elapsed().as_millis() as u64)? != bk {
            return Err("response peer mismatch".into());
        }
        let request = udp(
            aip,
            bip,
            format!("femto relay probe request {round}").as_bytes(),
        );
        ar.send(
            bk,
            &ap.send(bk, &request, started.elapsed().as_millis() as u64)?,
        )?;
        receive(&mut br, &mut bp, &mut bh, ak, &request, started)?;
        let response = udp(
            bip,
            aip,
            format!("femto relay probe response {round}").as_bytes(),
        );
        br.send(
            ak,
            &bp.send(ak, &response, started.elapsed().as_millis() as u64)?,
        )?;
        receive(&mut ar, &mut ap, &mut ah, bk, &response, started)?;
    }
    ar.close();
    br.close();
    ap.disconnect();
    bp.disconnect();
    ah.maintain(&mut ap, started.elapsed().as_millis() as u64)?;
    bh.maintain(&mut bp, started.elapsed().as_millis() as u64)?;
    println!(
        "{}",
        serde_json::json!({"result":"passed","relay":args[1],"address":address.to_string(),"unix_seconds":SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),"elapsed_ms":started.elapsed().as_millis(),"transport":"verified TLS + DERP","wireguard_handshake":"managed initiator/responder, confirmation and receiver-index routing","rounds":2,"timestamp_store":"temporary durable HostStore","packets":"bidirectional IPv4/UDP through receive policy","tailnet_enrolled":false})
    );
    Ok(())
}
#[cfg(not(all(unix, not(target_os = "espidf"))))]
fn main() {
    eprintln!("This diagnostic requires the Unix host TLS adapter.");
}
