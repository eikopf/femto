//! Opt-in public STUN probe. The observed public endpoint is not printed.
#[cfg(all(unix, not(target_os = "espidf")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use femto_microlink::{host_udp::UdpTransport, stun::Discovery};
    use std::{
        net::{SocketAddr, SocketAddrV4},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 2 {
        return Err("usage: stun-probe NUMERIC-IPV4:PORT".into());
    }
    let server: SocketAddrV4 = args[1].parse()?;
    let mut socket = UdpTransport::bind("0.0.0.0:0".parse()?)?;
    let mut discovery = Discovery::default();
    let started = Instant::now();
    let request = discovery.begin(server, 0)?;
    socket.send(SocketAddr::V4(server), &request)?;
    loop {
        let now = started.elapsed().as_millis() as u64;
        if now >= 5000 {
            return Err("STUN probe timed out without a matching valid response".into());
        }
        if let Some(datagram) = socket.receive()?
            && let SocketAddr::V4(source) = datagram.source
            && let Ok(observation) = discovery.finish(source, &datagram.bytes, now)
        {
            socket.close();
            println!(
                "{}",
                serde_json::json!({"result":"passed","server":server.to_string(),"round_trip_ms":observation.round_trip_ms,"observed_ipv4_endpoint":true,"public_endpoint_redacted":true,"unix_seconds":SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),"peer_authenticated":false})
            );
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[cfg(not(all(unix, not(target_os = "espidf"))))]
fn main() {
    eprintln!("This diagnostic requires the Unix host UDP adapter.");
}
