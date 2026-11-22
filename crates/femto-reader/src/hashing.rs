//! Incremental book hashing with a platform-selected SHA-256 backend.
use std::io;

#[cfg(all(target_os = "espidf", feature = "esp-idf-sha256"))]
pub const BACKEND: &str = "ESP-IDF mbedTLS";
#[cfg(not(all(target_os = "espidf", feature = "esp-idf-sha256")))]
pub const BACKEND: &str = "RustCrypto sha2";

#[cfg(all(target_os = "espidf", feature = "esp-idf-sha256"))]
pub(crate) use mbedtls::Hasher;
#[cfg(not(all(target_os = "espidf", feature = "esp-idf-sha256")))]
pub(crate) use portable::Hasher;

/// The SHA-256 digest of everything `reader` yields.
pub fn digest(mut reader: impl io::Read) -> io::Result<[u8; 32]> {
    let mut hasher = Hasher::new()?;
    let mut buffer = [0; 8192];
    loop {
        match reader.read(&mut buffer)? {
            0 => return hasher.finish(),
            count => hasher.update(&buffer[..count])?,
        }
    }
}

#[cfg(not(all(target_os = "espidf", feature = "esp-idf-sha256")))]
mod portable {
    use super::io;
    use sha2::{Digest, Sha256};

    pub(crate) struct Hasher(Sha256);
    impl Hasher {
        pub(crate) fn new() -> io::Result<Self> {
            Ok(Self(Sha256::new()))
        }
        pub(crate) fn update(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.0.update(bytes);
            Ok(())
        }
        pub(crate) fn finish(self) -> io::Result<[u8; 32]> {
            Ok(self.0.finalize().into())
        }
    }
}

#[cfg(any(
    all(test, not(target_os = "espidf")),
    all(target_os = "espidf", feature = "esp-idf-sha256")
))]
mod mbedtls {
    use super::io;
    #[cfg(all(test, not(target_os = "espidf")))]
    use super::test_sys as sys;
    #[cfg(target_os = "espidf")]
    use esp_idf_sys as sys;

    pub(crate) struct Hasher {
        context: sys::mbedtls_sha256_context,
        failed: bool,
    }

    fn check(code: i32) -> io::Result<()> {
        if code == 0 {
            Ok(())
        } else {
            Err(io::Error::other(format!("mbedTLS SHA-256 error {code}")))
        }
    }

    impl Hasher {
        pub(crate) fn new() -> io::Result<Self> {
            let mut context = std::mem::MaybeUninit::uninit();
            // The SDK initializes the complete context. Its matching free function
            // runs on every subsequent exit, including a failed start operation.
            unsafe { sys::mbedtls_sha256_init(context.as_mut_ptr()) };
            let mut hasher = Self {
                context: unsafe { context.assume_init() },
                failed: false,
            };
            check(unsafe { sys::mbedtls_sha256_starts(&mut hasher.context, 0) })?;
            Ok(hasher)
        }

        pub(crate) fn update(&mut self, bytes: &[u8]) -> io::Result<()> {
            if self.failed {
                return Err(io::Error::other("SHA-256 context failed"));
            }
            // On ESP32-S3, ESP-IDF saves/restores digest state and locks the shared
            // accelerator within this call, releasing it before the next file read.
            let result = check(unsafe {
                sys::mbedtls_sha256_update(&mut self.context, bytes.as_ptr(), bytes.len())
            });
            self.failed = result.is_err();
            result
        }

        pub(crate) fn finish(mut self) -> io::Result<[u8; 32]> {
            if self.failed {
                return Err(io::Error::other("SHA-256 context failed"));
            }
            let mut digest = [0; 32];
            check(unsafe { sys::mbedtls_sha256_finish(&mut self.context, digest.as_mut_ptr()) })?;
            Ok(digest)
        }
    }

    impl Drop for Hasher {
        fn drop(&mut self) {
            // The context is exclusively owned and is neither cloned nor exposed.
            unsafe { sys::mbedtls_sha256_free(&mut self.context) };
        }
    }
}

pub(crate) fn hex(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for &byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 15) as usize] as char);
    }
    output
}

// Host model of the SDK context lifecycle. It exercises the same Rust FFI owner,
// but does not simulate accelerator timing, DMA, or the SDK's internal lock.
#[cfg(all(test, not(target_os = "espidf")))]
mod test_sys {
    use sha2::{Digest, Sha256};
    use std::cell::Cell;

    thread_local! {
        pub static LIVE: Cell<usize> = const { Cell::new(0) };
        pub static FAIL: Cell<&'static str> = const { Cell::new("") };
    }
    #[allow(non_camel_case_types)]
    pub struct mbedtls_sha256_context(Sha256);

    pub unsafe fn mbedtls_sha256_init(context: *mut mbedtls_sha256_context) {
        unsafe { context.write(mbedtls_sha256_context(Sha256::new())) };
        LIVE.set(LIVE.get() + 1);
    }
    pub unsafe fn mbedtls_sha256_free(context: *mut mbedtls_sha256_context) {
        unsafe { (*context).0 = Sha256::new() };
        LIVE.set(LIVE.get() - 1);
    }
    pub unsafe fn mbedtls_sha256_starts(_: *mut mbedtls_sha256_context, is224: i32) -> i32 {
        assert_eq!(is224, 0);
        if FAIL.get() == "start" { -1 } else { 0 }
    }
    pub unsafe fn mbedtls_sha256_update(
        context: *mut mbedtls_sha256_context,
        bytes: *const u8,
        length: usize,
    ) -> i32 {
        if FAIL.get() == "update" {
            return -2;
        }
        unsafe {
            (*context)
                .0
                .update(std::slice::from_raw_parts(bytes, length))
        };
        0
    }
    pub unsafe fn mbedtls_sha256_finish(
        context: *mut mbedtls_sha256_context,
        output: *mut u8,
    ) -> i32 {
        if FAIL.get() == "finish" {
            return -3;
        }
        let digest = unsafe { (*context).0.clone().finalize() };
        unsafe { std::ptr::copy_nonoverlapping(digest.as_ptr(), output, 32) };
        0
    }
}

#[cfg(all(test, not(target_os = "espidf")))]
mod tests {
    use super::*;

    #[test]
    fn known_vectors_match_both_owners() {
        for (bytes, expected) in [
            (
                b"".as_slice(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc".as_slice(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".as_slice(),
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
        ] {
            let mut portable = portable::Hasher::new().unwrap();
            let mut sdk = mbedtls::Hasher::new().unwrap();
            portable.update(&[]).unwrap();
            sdk.update(&[]).unwrap();
            for chunk in bytes.chunks(7) {
                portable.update(chunk).unwrap();
                sdk.update(chunk).unwrap();
            }
            assert_eq!(hex(&portable.finish().unwrap()), expected);
            assert_eq!(hex(&sdk.finish().unwrap()), expected);
            assert_eq!(test_sys::LIVE.get(), 0);
        }
    }

    #[test]
    fn interleaved_contexts_preserve_independent_chunked_digests() {
        use sha2::{Digest, Sha256};
        let bytes: Vec<_> = (0..32799).map(|n| n as u8).collect();
        for chunk_size in [1, 63, 64, 65, 8192] {
            let mut first = mbedtls::Hasher::new().unwrap();
            let mut second = mbedtls::Hasher::new().unwrap();
            for chunk in bytes.chunks(chunk_size) {
                first.update(chunk).unwrap();
                second.update(b"abc").unwrap();
            }
            assert_eq!(
                first.finish().unwrap().as_slice(),
                Sha256::digest(&bytes).as_slice()
            );
            let repeated = b"abc".repeat(bytes.len().div_ceil(chunk_size));
            assert_eq!(
                second.finish().unwrap().as_slice(),
                Sha256::digest(&repeated).as_slice()
            );
            assert_eq!(test_sys::LIVE.get(), 0);
        }
    }

    #[test]
    fn sdk_errors_poison_updates_and_always_release_the_context() {
        test_sys::FAIL.set("start");
        assert!(mbedtls::Hasher::new().is_err());
        assert_eq!(test_sys::LIVE.get(), 0);
        test_sys::FAIL.set("");
        let mut sdk = mbedtls::Hasher::new().unwrap();
        test_sys::FAIL.set("update");
        assert!(sdk.update(b"abc").is_err());
        test_sys::FAIL.set("");
        assert!(sdk.update(b"def").is_err());
        assert!(sdk.finish().is_err());
        assert_eq!(test_sys::LIVE.get(), 0);
        let sdk = mbedtls::Hasher::new().unwrap();
        test_sys::FAIL.set("finish");
        assert!(sdk.finish().is_err());
        assert_eq!(test_sys::LIVE.get(), 0);
        test_sys::FAIL.set("");
        drop(mbedtls::Hasher::new().unwrap());
        assert_eq!(test_sys::LIVE.get(), 0);
    }
}
