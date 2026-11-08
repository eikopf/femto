#![cfg(feature = "native-mbedtls-tests")]
//! Real host Mbed TLS/OpenSSL qualification of the device Rust TLS owner.
extern crate self as esp_idf_svc;
use std::{
    ffi::CString,
    io::{self, BufRead, Read, Write},
    net::TcpStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[allow(non_camel_case_types)]
pub mod sys {
    use std::ffi::{c_char, c_void};
    #[repr(C)]
    pub struct Holder {
        pointer: *mut c_void,
    }
    pub type mbedtls_ssl_context = Holder;
    pub type mbedtls_ssl_config = Holder;
    pub type mbedtls_ctr_drbg_context = Holder;
    pub type mbedtls_entropy_context = Holder;
    pub const MBEDTLS_ERR_SSL_INTERNAL_ERROR: i32 = -27648;
    pub const MBEDTLS_ERR_SSL_PEER_CLOSE_NOTIFY: i32 = -30848;
    pub const MBEDTLS_ERR_SSL_WANT_READ: i32 = -26880;
    pub const MBEDTLS_ERR_SSL_WANT_WRITE: i32 = -26752;
    pub const MBEDTLS_SSL_IS_CLIENT: u32 = 0;
    pub const MBEDTLS_SSL_TRANSPORT_STREAM: u32 = 0;
    pub const MBEDTLS_SSL_PRESET_DEFAULT: u32 = 0;
    pub const MBEDTLS_SSL_VERIFY_REQUIRED: u32 = 2;
    type Rng = Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize) -> i32>;
    type Send = Option<unsafe extern "C" fn(*mut c_void, *const u8, usize) -> i32>;
    type Recv = Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize) -> i32>;
    type Timeout = Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize, u32) -> i32>;
    macro_rules! ffi {
        ($name:ident, $symbol:literal, ($($arg:ident: $ty:ty),*) $(-> $result:ty)?) => {
            unsafe extern "C" { #[link_name = $symbol] pub fn $name($($arg: $ty),*) $(-> $result)?; }
        }
    }
    ffi!(set_ca, "femto_set_ca", (path: *const c_char));
    ffi!(mbedtls_ssl_init, "femto_ssl_init", (p: *mut Holder));
    ffi!(mbedtls_ssl_config_init, "femto_ssl_config_init", (p: *mut Holder));
    ffi!(mbedtls_ctr_drbg_init, "femto_ctr_drbg_init", (p: *mut Holder));
    ffi!(mbedtls_entropy_init, "femto_entropy_init", (p: *mut Holder));
    ffi!(mbedtls_ssl_free, "femto_ssl_free", (p: *mut Holder));
    ffi!(mbedtls_ssl_config_free, "femto_ssl_config_free", (p: *mut Holder));
    ffi!(mbedtls_ctr_drbg_free, "femto_ctr_drbg_free", (p: *mut Holder));
    ffi!(mbedtls_entropy_free, "femto_entropy_free", (p: *mut Holder));
    ffi!(mbedtls_entropy_func, "femto_entropy_func", (p: *mut c_void, b: *mut u8, n: usize) -> i32);
    ffi!(mbedtls_ctr_drbg_random, "femto_ctr_drbg_random", (p: *mut c_void, b: *mut u8, n: usize) -> i32);
    ffi!(mbedtls_ctr_drbg_seed, "femto_ctr_drbg_seed", (p: *mut Holder, rng: Rng, context: *mut c_void, custom: *const u8, n: usize) -> i32);
    ffi!(mbedtls_ssl_config_defaults, "femto_ssl_config_defaults", (p: *mut Holder, endpoint: i32, transport: i32, preset: i32) -> i32);
    ffi!(mbedtls_ssl_conf_rng, "femto_ssl_conf_rng", (p: *mut Holder, rng: Rng, context: *mut c_void));
    ffi!(mbedtls_ssl_conf_authmode, "femto_ssl_conf_authmode", (p: *mut Holder, mode: i32));
    ffi!(mbedtls_ssl_conf_session_tickets, "femto_ssl_conf_session_tickets", (p: *mut Holder, mode: i32));
    ffi!(mbedtls_ssl_conf_renegotiation, "femto_ssl_conf_renegotiation", (p: *mut Holder, mode: i32));
    ffi!(esp_crt_bundle_attach, "femto_crt_bundle_attach", (p: *mut c_void) -> i32);
    ffi!(mbedtls_ssl_setup, "femto_ssl_setup", (p: *mut Holder, config: *const Holder) -> i32);
    ffi!(mbedtls_ssl_set_hostname, "femto_ssl_set_hostname", (p: *mut Holder, name: *const c_char) -> i32);
    ffi!(mbedtls_ssl_set_bio, "femto_ssl_set_bio", (p: *mut Holder, context: *mut c_void, send: Send, recv: Recv, timeout: Timeout));
    ffi!(mbedtls_ssl_handshake, "femto_ssl_handshake", (p: *mut Holder) -> i32);
    ffi!(mbedtls_ssl_get_verify_result, "femto_ssl_get_verify_result", (p: *const Holder) -> u32);
    ffi!(mbedtls_ssl_read, "femto_ssl_read", (p: *mut Holder, b: *mut u8, n: usize) -> i32);
    ffi!(mbedtls_ssl_write, "femto_ssl_write", (p: *mut Holder, b: *const u8, n: usize) -> i32);
}
mod clock {
    pub fn require_ready() -> std::io::Result<()> {
        Ok(())
    }
}
#[allow(unexpected_cfgs)]
#[path = "../../../firmware/picco/src/tailnet_tls.rs"]
mod tailnet_tls;
struct Socket {
    tcp: TcpStream,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
}
impl tailnet_tls::Socket for Socket {
    fn authorize(&mut self) -> io::Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if Instant::now() >= self.deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }
}
impl Read for Socket {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        use tailnet_tls::Socket as _;
        loop {
            self.authorize()?;
            match self.tcp.read(bytes) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                result => return result,
            }
        }
    }
}
impl Write for Socket {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        use tailnet_tls::Socket as _;
        self.authorize()?;
        self.tcp.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        use tailnet_tls::Socket as _;
        self.authorize()
    }
}
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn setup(certificate: &str, mode: &str) -> (Server, Socket, Arc<AtomicBool>) {
    let directory = PathBuf::from(
        std::env::var_os("FEMTO_TLS_FIXTURES").expect("Run scripts/check-native-tls"),
    );
    let ca = CString::new(directory.join("ca.pem").to_str().unwrap()).unwrap();
    unsafe {
        sys::set_ca(ca.as_ptr());
    }
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/native-tls/server.py");
    let child = Command::new("python")
        .arg(script)
        .arg(directory.join(format!("{certificate}.pem")))
        .arg(directory.join(format!("{certificate}.key")))
        .arg(mode)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut server = Server(child);
    let mut line = String::new();
    io::BufReader::new(server.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port: u16 = line.trim().parse().unwrap();
    let tcp = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
    tcp.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    (
        server,
        Socket {
            tcp,
            cancel: cancel.clone(),
            deadline: Instant::now() + Duration::from_secs(5),
        },
        cancel,
    )
}
#[test]
fn native_mbedtls_validates_chain_hostname_and_dates() {
    for (cert, name, valid) in [
        ("valid", "peer.example", true),
        ("valid", "wrong.example", false),
        ("expired", "peer.example", false),
        ("untrusted", "peer.example", false),
    ] {
        let (_server, socket, _) = setup(cert, "clean");
        let result = tailnet_tls::Tls::connect(socket, name);
        assert_eq!(
            result.is_ok(),
            valid,
            "{cert} {name}: {}",
            result.err().map(|e| e.to_string()).unwrap_or_default()
        );
    }
}
#[test]
fn native_mbedtls_transfers_plaintext_and_rejects_truncation() {
    for mode in ["clean", "truncated"] {
        let (_server, socket, _) = setup("valid", mode);
        let mut tls = tailnet_tls::Tls::connect(socket, "peer.example").unwrap();
        tls.write_all(b"GET / HTTP/1.1\r\nHost: peer.example\r\n\r\n")
            .unwrap();
        tls.flush().unwrap();
        let mut output = Vec::new();
        let result = tls.read_to_end(&mut output);
        if mode == "clean" {
            result.unwrap();
            assert_eq!(tls.read(&mut [0; 1]).unwrap(), 0);
        } else {
            assert!(
                result.is_err(),
                "raw TCP close must not become clean TLS EOF"
            );
        }
        assert_eq!(output, b"hello world".repeat(2000));
    }
}
#[test]
fn native_mbedtls_withholds_buffered_plaintext_after_cancellation() {
    let (_server, socket, cancel) = setup("valid", "clean");
    let mut tls = tailnet_tls::Tls::connect(socket, "peer.example").unwrap();
    tls.write_all(b"GET / HTTP/1.1\r\nHost: peer.example\r\n\r\n")
        .unwrap();
    let mut first = [0; 3];
    tls.read_exact(&mut first).unwrap();
    assert_eq!(&first, b"hel");
    cancel.store(true, Ordering::Release);
    let mut bytes = [0xa5; 16];
    assert_eq!(
        tls.read(&mut bytes).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(bytes, [0xa5; 16]);
    assert!(tls.write(b"late request").is_err());
}
