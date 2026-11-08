//! Verified Mbed TLS over managed tailnet TCP callbacks.
use esp_idf_svc::sys;
use std::{
    ffi::{CString, c_void},
    io::{self, Read, Write},
    time::Duration,
};

#[cfg(all(
    target_os = "espidf",
    not(all(esp_idf_mbedtls_certificate_bundle, esp_idf_mbedtls_have_time_date))
))]
compile_error!("Tailnet TLS requires the certificate bundle and certificate date verification");

/// The socket enforces its absolute deadline and cancellation in authorize and
/// every I/O call. Blocking I/O must remain inside that operation budget.
pub trait Socket: Read + Write {
    fn authorize(&mut self) -> io::Result<()>;
}
impl<D: femto_microlink::application_tcp::Driver> Socket
    for femto_microlink::application_tcp::Stream<D>
{
    fn authorize(&mut self) -> io::Result<()> {
        self.authorize()
    }
}
struct Bio<S> {
    socket: S,
    error: Option<io::Error>,
}
struct Session<S> {
    ssl: Box<sys::mbedtls_ssl_context>,
    config: Box<sys::mbedtls_ssl_config>,
    rng: Box<sys::mbedtls_ctr_drbg_context>,
    entropy: Box<sys::mbedtls_entropy_context>,
    bio: Box<Bio<S>>,
    eof: bool,
}
impl<S> Drop for Session<S> {
    fn drop(&mut self) {
        // SSL borrows config, RNG and BIO. Free SSL first, without close-notify
        // or any network I/O. The managed socket drops after these contexts.
        unsafe {
            sys::mbedtls_ssl_free(&mut *self.ssl);
            sys::mbedtls_ssl_config_free(&mut *self.config);
            sys::mbedtls_ctr_drbg_free(&mut *self.rng);
            sys::mbedtls_entropy_free(&mut *self.entropy);
        }
    }
}
// Each supplied Mbed TLS initializer writes the complete C context. Allocation
// occurs on the heap to avoid large temporary contexts on the worker stack.
unsafe fn initialized<T>(init: unsafe extern "C" fn(*mut T)) -> Box<T> {
    let mut value = Box::<T>::new_uninit();
    unsafe {
        init(value.as_mut_ptr());
        value.assume_init()
    }
}
fn tls_error(code: i32) -> io::Error {
    io::Error::other(format!("Tailnet TLS error {code}"))
}
fn check_code(code: i32) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(tls_error(code))
    }
}

unsafe extern "C" fn send<S: Socket>(context: *mut c_void, bytes: *const u8, length: usize) -> i32 {
    if length == 0 {
        return 0;
    }
    if context.is_null() || bytes.is_null() {
        return sys::MBEDTLS_ERR_SSL_INTERNAL_ERROR;
    }
    // Mbed TLS supplies its live BIO pointer and a readable record buffer.
    let bio = unsafe { &mut *context.cast::<Bio<S>>() };
    let bytes = unsafe { std::slice::from_raw_parts(bytes, length.min(i32::MAX as usize)) };
    callback(bio, |socket| socket.write(bytes))
}
unsafe extern "C" fn receive<S: Socket>(
    context: *mut c_void,
    bytes: *mut u8,
    length: usize,
) -> i32 {
    if length == 0 {
        return 0;
    }
    if context.is_null() || bytes.is_null() {
        return sys::MBEDTLS_ERR_SSL_INTERNAL_ERROR;
    }
    // Mbed TLS supplies a writable record buffer for this synchronous call.
    let bio = unsafe { &mut *context.cast::<Bio<S>>() };
    let bytes = unsafe { std::slice::from_raw_parts_mut(bytes, length.min(i32::MAX as usize)) };
    callback(bio, |socket| socket.read(bytes))
}
fn callback<S: Socket>(
    bio: &mut Bio<S>,
    operation: impl FnOnce(&mut S) -> io::Result<usize>,
) -> i32 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        bio.socket.authorize()?;
        crate::clock::require_ready()?;
        let count = operation(&mut bio.socket)?;
        bio.socket.authorize()?;
        crate::clock::require_ready()?;
        Ok::<_, io::Error>(count)
    }))
    .unwrap_or_else(|_| Err(io::Error::other("TLS I/O callback panicked")));
    match result {
        Ok(count) => count as i32,
        Err(error) => {
            bio.error = Some(error);
            sys::MBEDTLS_ERR_SSL_INTERNAL_ERROR
        }
    }
}

pub struct Tls<S: Socket> {
    inner: Option<Session<S>>,
}
#[derive(Clone, Copy)]
enum Operation {
    Handshake,
    Read,
    Write,
}
impl<S: Socket> Tls<S> {
    pub fn connect(socket: S, name: &str) -> io::Result<Self> {
        if name.is_empty()
            || name.len() > 253
            || !name.is_ascii()
            || name.bytes().any(|b| b.is_ascii_control())
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let name = CString::new(name).map_err(|_| io::ErrorKind::InvalidInput)?;
        crate::clock::require_ready()?;
        #[cfg(esp_idf_mbedtls_ssl_proto_tls1_3)]
        check_code(unsafe { sys::psa_crypto_init() })?;
        let session = Session {
            ssl: unsafe { initialized(sys::mbedtls_ssl_init) },
            config: unsafe { initialized(sys::mbedtls_ssl_config_init) },
            rng: unsafe { initialized(sys::mbedtls_ctr_drbg_init) },
            entropy: unsafe { initialized(sys::mbedtls_entropy_init) },
            bio: Box::new(Bio {
                socket,
                error: None,
            }),
            eof: false,
        };
        let mut result = Self {
            inner: Some(session),
        };
        let inner = result.inner.as_mut().unwrap();
        inner.bio.socket.authorize()?;
        // All contexts and BIO stay at stable heap addresses. The hostname is
        // copied by Mbed TLS. Certificate verification remains mandatory.
        unsafe {
            check_code(sys::mbedtls_ctr_drbg_seed(
                &mut *inner.rng,
                Some(sys::mbedtls_entropy_func),
                (&mut *inner.entropy as *mut sys::mbedtls_entropy_context).cast(),
                std::ptr::null(),
                0,
            ))?;
            check_code(sys::mbedtls_ssl_config_defaults(
                &mut *inner.config,
                sys::MBEDTLS_SSL_IS_CLIENT as i32,
                sys::MBEDTLS_SSL_TRANSPORT_STREAM as i32,
                sys::MBEDTLS_SSL_PRESET_DEFAULT as i32,
            ))?;
            sys::mbedtls_ssl_conf_rng(
                &mut *inner.config,
                Some(sys::mbedtls_ctr_drbg_random),
                (&mut *inner.rng as *mut sys::mbedtls_ctr_drbg_context).cast(),
            );
            sys::mbedtls_ssl_conf_authmode(
                &mut *inner.config,
                sys::MBEDTLS_SSL_VERIFY_REQUIRED as i32,
            );
            sys::mbedtls_ssl_conf_session_tickets(&mut *inner.config, 0);
            sys::mbedtls_ssl_conf_renegotiation(&mut *inner.config, 0);
            check_code(sys::esp_crt_bundle_attach(
                (&mut *inner.config as *mut sys::mbedtls_ssl_config).cast(),
            ))?;
            check_code(sys::mbedtls_ssl_setup(&mut *inner.ssl, &*inner.config))?;
            check_code(sys::mbedtls_ssl_set_hostname(
                &mut *inner.ssl,
                name.as_ptr(),
            ))?;
            sys::mbedtls_ssl_set_bio(
                &mut *inner.ssl,
                (&mut *inner.bio as *mut Bio<S>).cast(),
                Some(send::<S>),
                Some(receive::<S>),
                None,
            );
        }
        result.execute(Operation::Handshake, |inner| unsafe {
            sys::mbedtls_ssl_handshake(&mut *inner.ssl)
        })?;
        if unsafe { sys::mbedtls_ssl_get_verify_result(&*result.inner.as_ref().unwrap().ssl) } != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Tailnet TLS certificate verification failed",
            ));
        }
        result.boundary()?;
        Ok(result)
    }
    fn boundary(&mut self) -> io::Result<()> {
        let result = (|| {
            crate::clock::require_ready()?;
            self.inner
                .as_mut()
                .ok_or(io::ErrorKind::NotConnected)?
                .bio
                .socket
                .authorize()
        })();
        if result.is_err() {
            self.inner = None;
        }
        result
    }
    fn execute(
        &mut self,
        kind: Operation,
        mut operation: impl FnMut(&mut Session<S>) -> i32,
    ) -> io::Result<usize> {
        let result = (|| loop {
            self.boundary()?;
            let inner = self.inner.as_mut().unwrap();
            if matches!(kind, Operation::Read) && inner.eof {
                return Ok(0);
            }
            let code = operation(inner);
            if let Some(error) = inner.bio.error.take() {
                return Err(error);
            }
            self.boundary()?;
            if code == sys::MBEDTLS_ERR_SSL_WANT_READ || code == sys::MBEDTLS_ERR_SSL_WANT_WRITE {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            if matches!(kind, Operation::Read) && code == sys::MBEDTLS_ERR_SSL_PEER_CLOSE_NOTIFY {
                self.inner.as_mut().unwrap().eof = true;
                return Ok(0);
            }
            if code < 0 {
                return Err(tls_error(code));
            }
            if code == 0 {
                match kind {
                    Operation::Read => return Err(io::ErrorKind::UnexpectedEof.into()),
                    Operation::Write => return Err(io::ErrorKind::WriteZero.into()),
                    Operation::Handshake => (),
                }
            }
            return Ok(code as usize);
        })();
        if result.is_err() {
            self.inner = None;
        }
        result
    }
}
impl<S: Socket> Read for Tls<S> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            self.boundary()?;
            return Ok(0);
        }
        let mut staged = [0; 1024];
        let length = output.len().min(staged.len());
        let count = self.execute(Operation::Read, |inner| unsafe {
            sys::mbedtls_ssl_read(&mut *inner.ssl, staged.as_mut_ptr(), length)
        })?;
        output[..count].copy_from_slice(&staged[..count]);
        Ok(count)
    }
}
impl<S: Socket> Write for Tls<S> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.is_empty() {
            self.boundary()?;
            return Ok(0);
        }
        self.execute(Operation::Write, |inner| unsafe {
            sys::mbedtls_ssl_write(&mut *inner.ssl, input.as_ptr(), input.len().min(16384))
        })
    }
    fn flush(&mut self) -> io::Result<()> {
        self.boundary()
    }
}
