//! Actual device TLS owner with an injected Mbed TLS ABI, not a crypto interoperability test.
extern crate self as esp_idf_svc;
use std::{
    cell::RefCell,
    io::{self, Read, Write},
};
#[derive(Default)]
struct State {
    authorized: bool,
    clock_bad: bool,
    revoke_read: bool,
    read_code: Option<i32>,
    verify: u32,
    setup_error: i32,
    socket_error: bool,
    panic_write: bool,
    freed: usize,
    hostname: String,
    configured: bool,
    reads: usize,
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
#[allow(
    non_camel_case_types,
    clippy::missing_safety_doc,
    clippy::type_complexity
)]
pub mod sys {
    use super::*;
    use std::ffi::{c_char, c_void};
    pub const MBEDTLS_ERR_SSL_INTERNAL_ERROR: i32 = -1;
    pub const MBEDTLS_ERR_SSL_PEER_CLOSE_NOTIFY: i32 = -2;
    pub const MBEDTLS_ERR_SSL_WANT_READ: i32 = -3;
    pub const MBEDTLS_ERR_SSL_WANT_WRITE: i32 = -4;
    pub const MBEDTLS_SSL_IS_CLIENT: u32 = 0;
    pub const MBEDTLS_SSL_TRANSPORT_STREAM: u32 = 0;
    pub const MBEDTLS_SSL_PRESET_DEFAULT: u32 = 0;
    pub const MBEDTLS_SSL_VERIFY_REQUIRED: u32 = 2;
    type SendFn = Option<unsafe extern "C" fn(*mut c_void, *const u8, usize) -> i32>;
    type RecvFn = Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize) -> i32>;
    type RngFn = Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize) -> i32>;
    #[derive(Default)]
    pub struct mbedtls_ssl_context {
        config: *const mbedtls_ssl_config,
        bio: *mut c_void,
        send: SendFn,
        recv: RecvFn,
    }
    #[derive(Default)]
    pub struct mbedtls_ssl_config {
        auth: i32,
        bundle: bool,
        rng: bool,
        tickets: i32,
        renegotiate: i32,
    }
    pub struct mbedtls_ctr_drbg_context(u32);
    pub struct mbedtls_entropy_context(u32);
    pub unsafe extern "C" fn mbedtls_ssl_init(p: *mut mbedtls_ssl_context) {
        unsafe { p.write(Default::default()) };
    }
    pub unsafe extern "C" fn mbedtls_ssl_config_init(p: *mut mbedtls_ssl_config) {
        unsafe { p.write(Default::default()) };
    }
    pub unsafe extern "C" fn mbedtls_ctr_drbg_init(p: *mut mbedtls_ctr_drbg_context) {
        unsafe { p.write(mbedtls_ctr_drbg_context(0)) };
    }
    pub unsafe extern "C" fn mbedtls_entropy_init(p: *mut mbedtls_entropy_context) {
        unsafe { p.write(mbedtls_entropy_context(0)) };
    }
    pub unsafe fn mbedtls_ssl_free(_: *mut mbedtls_ssl_context) {
        STATE.with_borrow_mut(|s| s.freed += 1);
    }
    pub unsafe fn mbedtls_ssl_config_free(_: *mut mbedtls_ssl_config) {
        STATE.with_borrow_mut(|s| s.freed += 1);
    }
    pub unsafe fn mbedtls_ctr_drbg_free(p: *mut mbedtls_ctr_drbg_context) {
        let _ = unsafe { (*p).0 };
        STATE.with_borrow_mut(|s| s.freed += 1);
    }
    pub unsafe fn mbedtls_entropy_free(p: *mut mbedtls_entropy_context) {
        let _ = unsafe { (*p).0 };
        STATE.with_borrow_mut(|s| s.freed += 1);
    }
    pub unsafe extern "C" fn mbedtls_entropy_func(_: *mut c_void, _: *mut u8, _: usize) -> i32 {
        0
    }
    pub unsafe extern "C" fn mbedtls_ctr_drbg_random(_: *mut c_void, _: *mut u8, _: usize) -> i32 {
        0
    }
    pub unsafe fn mbedtls_ctr_drbg_seed(
        _: *mut mbedtls_ctr_drbg_context,
        _: RngFn,
        _: *mut c_void,
        _: *const u8,
        _: usize,
    ) -> i32 {
        0
    }
    pub unsafe fn mbedtls_ssl_config_defaults(
        _: *mut mbedtls_ssl_config,
        _: i32,
        _: i32,
        _: i32,
    ) -> i32 {
        STATE.with_borrow(|s| s.setup_error)
    }
    pub unsafe fn mbedtls_ssl_conf_rng(p: *mut mbedtls_ssl_config, f: RngFn, _: *mut c_void) {
        unsafe { (*p).rng = f.is_some() };
    }
    pub unsafe fn mbedtls_ssl_conf_authmode(p: *mut mbedtls_ssl_config, mode: i32) {
        unsafe { (*p).auth = mode };
    }
    pub unsafe fn mbedtls_ssl_conf_session_tickets(p: *mut mbedtls_ssl_config, mode: i32) {
        unsafe { (*p).tickets = mode };
    }
    pub unsafe fn mbedtls_ssl_conf_renegotiation(p: *mut mbedtls_ssl_config, mode: i32) {
        unsafe { (*p).renegotiate = mode };
    }
    pub unsafe fn esp_crt_bundle_attach(p: *mut c_void) -> i32 {
        unsafe { (*p.cast::<mbedtls_ssl_config>()).bundle = true };
        0
    }
    pub unsafe fn mbedtls_ssl_setup(
        p: *mut mbedtls_ssl_context,
        c: *const mbedtls_ssl_config,
    ) -> i32 {
        unsafe { (*p).config = c };
        0
    }
    pub unsafe fn mbedtls_ssl_set_hostname(
        _: *mut mbedtls_ssl_context,
        name: *const c_char,
    ) -> i32 {
        let name = unsafe { std::ffi::CStr::from_ptr(name) }.to_str().unwrap();
        STATE.with_borrow_mut(|s| s.hostname = name.into());
        0
    }
    pub unsafe fn mbedtls_ssl_set_bio(
        p: *mut mbedtls_ssl_context,
        bio: *mut c_void,
        send: SendFn,
        recv: RecvFn,
        _: Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize, u32) -> i32>,
    ) {
        let p = unsafe { &mut *p };
        p.bio = bio;
        p.send = send;
        p.recv = recv;
    }
    pub unsafe fn mbedtls_ssl_handshake(p: *mut mbedtls_ssl_context) -> i32 {
        let p = unsafe { &mut *p };
        let config = unsafe { &*p.config };
        STATE.with_borrow_mut(|s| {
            s.configured = config.auth == 2
                && config.bundle
                && config.rng
                && config.tickets == 0
                && config.renegotiate == 0
        });
        let sent = unsafe { p.send.unwrap()(p.bio, b"hello".as_ptr(), 5) };
        if sent < 0 {
            return sent;
        }
        let mut byte = [0];
        let read = unsafe { p.recv.unwrap()(p.bio, byte.as_mut_ptr(), 1) };
        if read < 0 { read } else { 0 }
    }
    pub unsafe fn mbedtls_ssl_get_verify_result(_: *const mbedtls_ssl_context) -> u32 {
        STATE.with_borrow(|s| s.verify)
    }
    pub unsafe fn mbedtls_ssl_read(_: *mut mbedtls_ssl_context, p: *mut u8, len: usize) -> i32 {
        STATE.with_borrow_mut(|s| {
            s.reads += 1;
            if s.revoke_read {
                s.authorized = false;
            }
            if let Some(code) = s.read_code {
                return code;
            }
            let n = len.min(5);
            unsafe { std::ptr::copy_nonoverlapping(b"hello".as_ptr(), p, n) };
            n as i32
        })
    }
    pub unsafe fn mbedtls_ssl_write(
        p: *mut mbedtls_ssl_context,
        bytes: *const u8,
        len: usize,
    ) -> i32 {
        let p = unsafe { &mut *p };
        unsafe { p.send.unwrap()(p.bio, bytes, len) }
    }
}
#[allow(unexpected_cfgs)]
#[path = "../../../firmware/picco/src/tailnet_tls.rs"]
mod tailnet_tls;
struct Socket;
impl Read for Socket {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        b[0] = 1;
        Ok(1)
    }
}
impl Write for Socket {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if STATE.with_borrow(|s| s.panic_write) {
            panic!("injected callback panic");
        }
        if STATE.with_borrow(|s| s.socket_error) {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl tailnet_tls::Socket for Socket {
    fn authorize(&mut self) -> io::Result<()> {
        if STATE.with_borrow(|s| s.authorized) {
            Ok(())
        } else {
            Err(io::ErrorKind::ConnectionAborted.into())
        }
    }
}
fn reset() {
    STATE.with_borrow_mut(|s| {
        *s = State {
            authorized: true,
            ..Default::default()
        }
    });
}
#[test]
fn configuration_and_buffered_plaintext_authority() {
    reset();
    let mut tls = tailnet_tls::Tls::connect(Socket, "peer.example").unwrap();
    assert!(STATE.with_borrow(|s| s.configured));
    assert_eq!(STATE.with_borrow(|s| s.hostname.clone()), "peer.example");
    let mut bytes = [0; 5];
    tls.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"hello");
    STATE.with_borrow_mut(|s| s.revoke_read = true);
    bytes.fill(0xa5);
    assert_eq!(
        tls.read(&mut bytes).unwrap_err().kind(),
        io::ErrorKind::ConnectionAborted
    );
    assert_eq!(bytes, [0xa5; 5]);
    assert!(tls.write(b"request").is_err());
    assert_eq!(STATE.with_borrow(|s| s.freed), 4);
}
#[test]
fn setup_callback_and_certificate_failures_release_all_contexts() {
    for mode in 0..4 {
        reset();
        STATE.with_borrow_mut(|s| match mode {
            0 => s.setup_error = -42,
            1 => s.verify = 1,
            2 => s.socket_error = true,
            _ => s.panic_write = true,
        });
        let error = tailnet_tls::Tls::connect(Socket, "peer.example")
            .err()
            .unwrap();
        if mode == 1 {
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }
        if mode == 2 {
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        }
        assert_eq!(STATE.with_borrow(|s| s.freed), 4);
    }
}
#[test]
fn clean_notify_differs_from_truncation_and_clock_loss() {
    for code in [sys::MBEDTLS_ERR_SSL_PEER_CLOSE_NOTIFY, 0] {
        reset();
        let mut tls = tailnet_tls::Tls::connect(Socket, "peer.example").unwrap();
        STATE.with_borrow_mut(|s| s.read_code = Some(code));
        let mut bytes = [0xa5; 8];
        if code == 0 {
            assert_eq!(
                tls.read(&mut bytes).unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
        } else {
            assert_eq!(tls.read(&mut bytes).unwrap(), 0);
            assert_eq!(tls.read(&mut bytes).unwrap(), 0);
            assert_eq!(STATE.with_borrow(|s| s.reads), 1);
        }
        assert_eq!(bytes, [0xa5; 8]);
    }
    reset();
    let mut tls = tailnet_tls::Tls::connect(Socket, "peer.example").unwrap();
    STATE.with_borrow_mut(|s| s.clock_bad = true);
    assert!(tls.flush().is_err());
    assert_eq!(STATE.with_borrow(|s| s.freed), 4);
}
