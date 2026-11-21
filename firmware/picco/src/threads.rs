//! Where thread stacks live.
//!
//! Internal RAM cannot hold the 64 KiB and larger stacks of network
//! workers beside the drivers, so worker stacks are allocated in external
//! RAM. External RAM is unreachable while the flash cache is disabled, and
//! ESP-IDF asserts that flash operations run on an internal stack; code that
//! touches flash, such as NVS access, goes through [`on_internal_stack`].
use esp_idf_svc::sys;

fn configure(caps: u32, stack_size: usize, inherit: bool) -> Result<(), String> {
    let mut config = unsafe { sys::esp_pthread_get_default_config() };
    config.stack_alloc_caps = caps | sys::MALLOC_CAP_8BIT;
    config.stack_size = stack_size;
    config.inherit_cfg = inherit;
    sys::esp!(unsafe { sys::esp_pthread_set_cfg(&config) }).map_err(|e| e.to_string())
}

/// Threads created by the calling thread, and by those threads in turn,
/// get their stacks in external RAM. An explicit stack size still applies.
pub fn use_external_stacks() -> Result<(), String> {
    configure(sys::MALLOC_CAP_SPIRAM, 16 * 1024, true)
}

/// Whether the calling thread's stack is in internal RAM.
fn on_internal() -> bool {
    // ESP32-S3 internal data RAM; external RAM is mapped below it.
    const INTERNAL: std::ops::Range<usize> = 0x3FC8_8000..0x3FD0_0000;
    let probe = 0u8;
    INTERNAL.contains(&std::ptr::from_ref(&probe).addr())
}

/// Runs `operation` on a stack in internal RAM: directly when the caller
/// already has one, otherwise on a short-lived helper thread.
pub fn on_internal_stack<R: Send>(operation: impl FnOnce() -> R + Send) -> R {
    if on_internal() {
        return operation();
    }
    let mut previous = sys::esp_pthread_cfg_t::default();
    let had_previous = unsafe { sys::esp_pthread_get_cfg(&mut previous) } == sys::ESP_OK;
    if let Err(error) = configure(sys::MALLOC_CAP_INTERNAL, 8 * 1024, false) {
        // Without a helper the operation would assert inside ESP-IDF.
        panic!("cannot create an internal-stack helper: {error}");
    }
    let result = std::thread::scope(|scope| {
        scope
            .spawn(operation)
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    });
    if had_previous {
        unsafe { sys::esp_pthread_set_cfg(&previous) };
    }
    result
}
