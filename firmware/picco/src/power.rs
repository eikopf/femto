use esp_idf_svc::sys;
use std::time::{Duration, Instant};
fn check(code: i32) -> Result<(), String> {
    if code == 0 {
        Ok(())
    } else {
        Err(format!("Power control error {code}"))
    }
}
pub fn release_holds() -> Result<(), String> {
    unsafe {
        sys::gpio_deep_sleep_hold_dis();
    }
    check(unsafe { sys::gpio_hold_dis(4) })?;
    check(unsafe { sys::gpio_set_level(4, 1) })?;
    check(unsafe { sys::gpio_set_direction(4, sys::gpio_mode_t_GPIO_MODE_OUTPUT) })?;
    for pin in [0, 16, 41, 42, 40, 39, 44, 43] {
        check(unsafe { sys::gpio_hold_dis(pin) })?;
    }
    check(unsafe { sys::rtc_gpio_deinit(2) })
}
pub fn wait_for_release() -> Result<(), String> {
    let start = Instant::now();
    let mut released = None;
    while start.elapsed() < Duration::from_secs(5) {
        if unsafe { sys::gpio_get_level(2) } != 0 {
            let since = released.get_or_insert_with(Instant::now);
            if since.elapsed() >= Duration::from_millis(50) {
                return Ok(());
            }
        } else {
            released = None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err("Sleep cancelled: power button remains pressed".into())
}
fn hold(pin: i32, high: bool) -> Result<(), String> {
    check(unsafe { sys::gpio_hold_dis(pin) })?;
    check(unsafe { sys::gpio_set_level(pin, u32::from(high)) })?;
    check(unsafe { sys::gpio_set_direction(pin, sys::gpio_mode_t_GPIO_MODE_OUTPUT) })?;
    check(unsafe { sys::gpio_hold_en(pin) })
}
/// Called only after saving, controller shutdown, file closure, and driver teardown.
/// The power button wakes the device, and so does the timer when one is given.
pub fn enter(wake_after: Option<Duration>) -> ! {
    let result = (|| -> Result<(), String> {
        if let Some(wake_after) = wake_after {
            check(unsafe { sys::esp_sleep_enable_timer_wakeup(wake_after.as_micros() as u64) })?;
        }
        check(unsafe {
            sys::esp_sleep_enable_ext1_wakeup(
                1 << 2,
                sys::esp_sleep_ext1_wakeup_mode_t_ESP_EXT1_WAKEUP_ANY_LOW,
            )
        })?;
        check(unsafe {
            sys::esp_sleep_pd_config(
                sys::esp_sleep_pd_domain_t_ESP_PD_DOMAIN_RTC_PERIPH,
                sys::esp_sleep_pd_option_t_ESP_PD_OPTION_ON,
            )
        })?;
        check(unsafe { sys::rtc_gpio_pulldown_dis(2) })?;
        check(unsafe { sys::rtc_gpio_pullup_en(2) })?;
        hold(4, true)?;
        for pin in [41, 42, 40, 39, 44, 43, 16] {
            hold(pin, false)?;
        }
        hold(0, true)?;
        unsafe {
            sys::esp_sleep_config_gpio_isolate();
            sys::gpio_deep_sleep_hold_en();
            sys::esp_deep_sleep_start();
        }
        #[allow(unreachable_code)]
        Err("Deep sleep returned unexpectedly".into())
    })();
    log::error!("Sleep entry failed; restarting: {result:?}");
    unsafe { sys::esp_restart() }
}

/// Keeps the processor out of light sleep while held. Between polls the
/// reader has nothing to do for most of every 10 ms, and without this held
/// the processor sleeps through it; a USB host loses the device when it
/// does, so the reader holds this while one is attached.
pub struct Awake {
    lock: sys::esp_pm_lock_handle_t,
    held: bool,
}
impl Awake {
    /// Allows light sleep between polls and frequency scaling, and starts
    /// out holding the processor awake.
    pub fn open() -> Result<Self, String> {
        let mut lock = std::ptr::null_mut();
        check(unsafe {
            sys::esp_pm_lock_create(
                sys::esp_pm_lock_type_t_ESP_PM_NO_LIGHT_SLEEP,
                0,
                c"femto".as_ptr(),
                &mut lock,
            )
        })?;
        check(unsafe { sys::esp_pm_lock_acquire(lock) })?;
        // The peripheral clock stays at 80 MHz down to this processor
        // frequency, so no bus timing changes with it.
        let config = sys::esp_pm_config_t {
            max_freq_mhz: 240,
            min_freq_mhz: 80,
            light_sleep_enable: true,
        };
        check(unsafe { sys::esp_pm_configure(std::ptr::from_ref(&config).cast()) })?;
        Ok(Self { lock, held: true })
    }
    pub fn hold(&mut self, awake: bool) {
        if awake == self.held {
            return;
        }
        let code = unsafe {
            if awake {
                sys::esp_pm_lock_acquire(self.lock)
            } else {
                sys::esp_pm_lock_release(self.lock)
            }
        };
        if code == sys::ESP_OK {
            self.held = awake;
        }
    }
}

/// The power-management report: locks, and time in each mode since start.
fn modes() -> Option<String> {
    let (mut text, mut len) = (std::ptr::null_mut(), 0);
    unsafe {
        let stream = sys::open_memstream(&mut text, &mut len);
        if stream.is_null() {
            return None;
        }
        sys::esp_pm_dump_locks(stream);
        sys::fclose(stream);
        let report = std::slice::from_raw_parts(text.cast::<u8>(), len);
        let report = String::from_utf8_lossy(report).into_owned();
        sys::free(text.cast());
        Some(report)
    }
}

/// Logs how long the processor has spent in each power mode since start.
pub fn report_modes() {
    for line in modes().iter().flat_map(|report| report.lines()) {
        log::info!("Power: {line}");
    }
}

/// Microseconds the processor has spent in light sleep since start.
pub fn light_sleep_micros() -> Option<u64> {
    let report = modes()?;
    let line = report.lines().find(|line| line.starts_with("SLEEP"))?;
    // Mode, frequency and its unit, then the time.
    line.split_whitespace().nth(3)?.parse().ok()
}
