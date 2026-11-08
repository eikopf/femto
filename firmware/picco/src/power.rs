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
pub fn enter() -> ! {
    let result = (|| -> Result<(), String> {
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
