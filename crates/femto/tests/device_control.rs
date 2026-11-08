//! Device bootstrap controller with an injected transport and clock.
use std::{
    cell::RefCell,
    io::{self, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Default)]
struct State {
    calls: usize,
    certificate: String,
    address: Option<std::net::SocketAddr>,
    url: String,
    request: Vec<u8>,
    cancel_read: bool,
    pending: bool,
    clock_bad: bool,
    wire: Vec<u8>,
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }
mod clock {
    pub fn require_ready() -> std::io::Result<()> {
        if super::STATE.with_borrow(|s| s.clock_bad) {
            Err(std::io::Error::other("clock unavailable"))
        } else {
            Ok(())
        }
    }
}
mod network {
    use super::*;
    pub struct Transport;
    pub struct Reply {
        reader: io::Cursor<Vec<u8>>,
        cancel: Arc<AtomicBool>,
    }
    pub fn connect_tls_at(
        url: &femto_catalog::http::Url,
        address: std::net::SocketAddr,
        name: &str,
        deadline: Instant,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<Connection> {
        STATE.with_borrow_mut(|s| {
            s.certificate = name.into();
            s.address = Some(address);
        });
        connect(url, deadline, cancel)
    }
    pub type Connection = Reply;
    impl Reply {
        pub fn into_nonblocking(
            self,
        ) -> io::Result<(Box<dyn femto_catalog::http::Stream + Send>, Arc<AtomicBool>)> {
            let cancel = self.cancel.clone();
            Ok((Box::new(self), cancel))
        }
    }
    impl femto_microlink::derp::DeadlineIo for Reply {
        fn set_deadline(&mut self, _: Instant) {}
    }
    pub fn connect(
        url: &femto_catalog::http::Url,
        _: Instant,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<Connection> {
        assert_eq!(url.scheme(), "https");
        let wire = STATE.with_borrow_mut(|s| {
            s.calls += 1;
            s.url = url.to_string();
            s.wire.clone()
        });
        Ok(Reply {
            reader: io::Cursor::new(wire),
            cancel,
        })
    }
    impl Read for Reply {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if STATE.with_borrow(|s| s.pending) {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let n = self.reader.read(buffer)?;
            if STATE.with_borrow(|s| s.cancel_read) {
                self.cancel.store(true, Ordering::Release);
            }
            Ok(n)
        }
    }
    impl Write for Reply {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if STATE.with_borrow(|s| s.pending) {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            STATE.with_borrow_mut(|s| s.request.extend_from_slice(bytes));
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl femto_catalog::http::Transport for Transport {
        fn connect(
            &mut self,
            url: &femto_catalog::http::Url,
            _: Instant,
            cancel: Arc<AtomicBool>,
        ) -> io::Result<Box<dyn femto_catalog::http::Stream>> {
            assert_eq!(url.scheme(), "https");
            let wire = STATE.with_borrow_mut(|s| {
                s.calls += 1;
                s.url = url.to_string();
                s.wire.clone()
            });
            Ok(Box::new(Reply {
                reader: io::Cursor::new(wire),
                cancel,
            }))
        }
    }
}
#[path = "../../../firmware/picco/src/control.rs"]
mod control;
fn setup() {
    let body = format!(r#"{{"publicKey":"mkey:{}"}}"#, "01".repeat(32));
    STATE.with_borrow_mut(|s| {
        *s = State {
            wire: format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .into_bytes(),
            ..Default::default()
        }
    });
}
fn fetch(
    host: &str,
    port: u16,
    cancel: Arc<AtomicBool>,
) -> io::Result<femto_microlink::protocol::key::PublicKey<femto_microlink::protocol::key::Machine>>
{
    control::fetch_control_key(
        host,
        port,
        123,
        Instant::now() + Duration::from_secs(1),
        cancel,
    )
}
#[test]
fn bootstrap_uses_https_and_the_matching_authority() {
    setup();
    let key = fetch("control.example", 8443, Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(key.to_string(), format!("mkey:{}", "01".repeat(32)));
    STATE.with_borrow(|s| {
        assert_eq!(s.url, "https://control.example:8443/");
        assert!(
            String::from_utf8_lossy(&s.request)
                .starts_with("GET /key?v=123 HTTP/1.1\r\nHost: control.example:8443\r\n")
        );
    });
}
#[test]
fn invalid_destination_and_expired_requests_do_not_connect() {
    setup();
    for (host, port) in [
        ("", 443),
        ("bad\r\nhost", 443),
        ("name/path", 443),
        ("valid.example", 0),
    ] {
        assert!(fetch(host, port, Arc::new(AtomicBool::new(false))).is_err());
    }
    assert!(fetch("valid.example", 443, Arc::new(AtomicBool::new(true))).is_err());
    assert!(
        control::fetch_control_key(
            "valid.example",
            443,
            1,
            Instant::now(),
            Arc::new(AtomicBool::new(false))
        )
        .is_err()
    );
    assert_eq!(STATE.with_borrow(|s| s.calls), 0);
}
#[test]
fn late_cancel_clock_failure_and_redirects_withhold_keys() {
    for condition in 0..3 {
        setup();
        STATE.with_borrow_mut(|s| match condition {
            0 => s.cancel_read = true,
            1 => s.clock_bad = true,
            _ => s.wire = b"HTTP/1.1 302 Found\r\nLocation: https://other.example/\r\nContent-Length: 0\r\n\r\n".to_vec(),
        });
        assert!(fetch("control.example", 443, Arc::new(AtomicBool::new(false))).is_err());
        assert_eq!(STATE.with_borrow(|s| s.calls), 1);
    }
}

#[test]
fn device_noise_upgrade_rejects_non_upgrade_response() {
    setup();
    let identity = femto_microlink::protocol::identity::DeviceIdentity::generate().unwrap();
    let server = femto_microlink::protocol::identity::DeviceIdentity::generate().unwrap();
    let endpoint = femto_microlink::control_session::Endpoint {
        host: "control.example".into(),
        port: 443,
        control_key: server.machine_key().public(),
        version: 123,
    };
    assert!(
        control::connect_session(
            &endpoint,
            identity.machine_key(),
            Duration::from_secs(1),
            Arc::new(AtomicBool::new(false))
        )
        .is_err()
    );
    assert_eq!(STATE.with_borrow(|s| s.calls), 1);
}

#[path = "../../../firmware/picco/src/relay.rs"]
mod relay;
fn relay_node() -> femto_microlink::relay::RelayNode {
    use femto_microlink::relay::{Address, Certificate, RelayNode};
    RelayNode {
        name: "test".into(),
        host_name: "relay.example".into(),
        certificate: Certificate::Name("certificate.example".into()),
        ipv4: Address::Fixed("192.0.2.1".parse().unwrap()),
        ipv6: Address::Disabled,
        tls_port: 443,
        stun_port: None,
        stun_only: false,
    }
}
#[test]
fn device_relay_passes_selected_address_and_certificate_name_to_tls() {
    setup();
    let identity = femto_microlink::protocol::identity::DeviceIdentity::generate().unwrap();
    let address = "192.0.2.1:443".parse().unwrap();
    // A plain HTTP response cannot complete the authenticated DERP handshake.
    assert!(
        relay::connect(
            &relay_node(),
            address,
            identity.node_key().clone(),
            Duration::from_secs(1),
            Arc::new(AtomicBool::new(false))
        )
        .is_err()
    );
    STATE.with_borrow(|s| {
        assert_eq!(s.calls, 1);
        assert_eq!(s.address, Some(address));
        assert_eq!(s.certificate, "certificate.example");
        assert_eq!(s.url, "https://relay.example/");
    });
}
#[test]
fn disallowed_relay_targets_and_pins_never_connect() {
    use femto_microlink::relay::{Address, Certificate};
    let identity = femto_microlink::protocol::identity::DeviceIdentity::generate().unwrap();
    for condition in 0..8 {
        setup();
        let mut node = relay_node();
        let mut address = "192.0.2.1:443".parse().unwrap();
        match condition {
            0 => node.ipv4 = Address::Disabled,
            1 => address = "192.0.2.2:443".parse().unwrap(),
            2 => address = "192.0.2.1:444".parse().unwrap(),
            3 => node.stun_only = true,
            4 => node.certificate = Certificate::Sha256([0; 32]),
            5 => {
                node.ipv4 = Address::Dns;
                address = "0.0.0.0:443".parse().unwrap();
            }
            6 => {
                node.ipv4 = Address::Dns;
                address = "224.0.0.1:443".parse().unwrap();
            }
            _ => address = "[::1]:443".parse().unwrap(),
        }
        assert!(
            relay::connect(
                &node,
                address,
                identity.node_key().clone(),
                Duration::from_secs(1),
                Arc::new(AtomicBool::new(false))
            )
            .is_err()
        );
        assert_eq!(STATE.with_borrow(|s| s.calls), 0);
    }
}

#[path = "../../../firmware/picco/src/relay_transport.rs"]
mod relay_transport;

#[test]
fn nonblocking_relay_io_preserves_pending_and_withholds_cancelled_reads() {
    use femto_microlink::relay_pool::Transport as _;
    STATE.with_borrow_mut(|s| {
        *s = State {
            wire: b"hello".to_vec(),
            pending: true,
            ..Default::default()
        }
    });
    let cancel = Arc::new(AtomicBool::new(false));
    let stream = network::connect(
        &femto_catalog::http::Url::parse("https://relay.example").unwrap(),
        Instant::now() + Duration::from_secs(1),
        cancel,
    )
    .unwrap();
    let mut tls = relay_transport::Tls::from_verified(stream).unwrap();
    let mut output = [0xa5; 5];
    assert!(tls.receive(&mut output).unwrap().is_none());
    assert_eq!(tls.queue(b"packet").unwrap(), 0);
    assert_eq!(output, [0xa5; 5]);
    STATE.with_borrow_mut(|s| {
        s.pending = false;
        s.cancel_read = true;
    });
    assert_eq!(
        tls.receive(&mut output).unwrap_err().kind(),
        io::ErrorKind::ConnectionAborted
    );
    assert_eq!(output, [0xa5; 5]);
    assert!(tls.check().is_err());
    assert!(tls.queue(b"packet").is_err());
    assert!(tls.flush_pending().is_err());
    let pool = relay_transport::Pool::default();
    assert!(pool.is_empty());
    let _connect = relay_transport::connect;
}
