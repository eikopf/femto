use esp_idf_svc::{
    hal::{
        adc::{
            attenuation::DB_12,
            oneshot::{
                AdcChannelDriver, AdcDriver,
                config::{AdcChannelConfig, Calibration},
            },
        },
        gpio::{AnyInputPin, AnyOutputPin, Input, Output, PinDriver, Pull},
        i2c::{I2cDriver, config::Config as I2cConfig},
        peripherals::Peripherals,
        spi::{SpiDeviceDriver, SpiDriver, config},
        units::Hertz,
    },
    sys,
};
use femto_picco_board::{Bus, Display, Panel, Refresh, identify};
use std::time::{Duration, Instant};

fn delay(ms: u32) {
    std::thread::sleep(Duration::from_millis(ms.into()));
}

fn check(code: i32) -> Result<(), String> {
    if code == 0 {
        Ok(())
    } else {
        Err(format!("GPIO error {code}"))
    }
}

// Raw GPIO access is confined to startup before the HAL owns display pins.
fn set(pin: i32, high: bool) -> Result<(), String> {
    check(unsafe { sys::gpio_set_level(pin, u32::from(high)) })
}

fn direction(pin: i32, output: bool) -> Result<(), String> {
    check(unsafe {
        sys::gpio_set_direction(
            pin,
            if output {
                sys::gpio_mode_t_GPIO_MODE_OUTPUT
            } else {
                sys::gpio_mode_t_GPIO_MODE_INPUT
            },
        )
    })
}

fn pulse() -> Result<(), String> {
    set(48, true)?;
    unsafe { sys::esp_rom_delay_us(5) };
    set(48, false)?;
    unsafe { sys::esp_rom_delay_us(5) };
    Ok(())
}

fn read_id(command: u8, output: &mut [u8]) -> Result<(), String> {
    set(16, false)?;
    delay(10);
    set(16, true)?;
    delay(40);
    set(18, false)?;
    let result = (|| {
        set(17, false)?;
        for bit in (0..8).rev() {
            set(47, command & (1 << bit) != 0)?;
            pulse()?;
        }
        direction(47, false)?;
        check(unsafe { sys::gpio_set_pull_mode(47, sys::gpio_pull_mode_t_GPIO_PULLUP_ONLY) })?;
        set(17, true)?;
        unsafe { sys::esp_rom_delay_us(5) };
        for byte in output {
            *byte = 0;
            for _ in 0..8 {
                *byte = (*byte << 1) | u8::from(unsafe { sys::gpio_get_level(47) } != 0);
                pulse()?;
            }
        }
        Ok(())
    })();
    let release = set(18, true);
    let restore = direction(47, true);
    result.and(release).and(restore)
}

fn probe() -> Result<Panel, String> {
    crate::power::release_holds()?;
    // Picco shared peripheral rail is active-low; GPIO4 retains board power.
    for (pin, high) in [
        (4, true),
        (0, false),
        (18, true),
        (17, true),
        (48, false),
        (47, false),
        (16, true),
    ] {
        check(unsafe { sys::gpio_hold_dis(pin) })?;
        set(pin, high)?;
        direction(pin, true)?;
    }
    delay(50);
    check(unsafe { sys::gpio_hold_dis(15) })?;
    let mut id = [0; 10];
    read_id(0x2e, &mut id)?;
    let mut revision = [0; 3];
    if identify(id, revision).is_none() {
        read_id(0x70, &mut revision)?;
    }
    log::info!("Panel user ID: {id:02x?}, revision: {revision:02x?}");
    identify(id, revision).ok_or_else(|| "Unknown Picco panel ID".into())
}

struct PanelBus {
    spi: SpiDeviceDriver<'static, SpiDriver<'static>>,
    cs: PinDriver<'static, Output>,
    dc: PinDriver<'static, Output>,
    reset: PinDriver<'static, Output>,
    busy: PinDriver<'static, Input>,
    busy_active_high: bool,
}

impl Bus for PanelBus {
    type Error = String;
    fn reset(&mut self) -> Result<(), String> {
        for high in [true, false, true] {
            let result = if high {
                self.reset.set_high()
            } else {
                self.reset.set_low()
            };
            result.map_err(|e| e.to_string())?;
            delay(10);
        }
        Ok(())
    }
    fn command(&mut self, command: u8, data: &[u8]) -> Result<(), String> {
        self.cs.set_low().map_err(|e| e.to_string())?;
        let result = (|| {
            self.dc.set_low()?;
            self.spi.write(&[command])?;
            self.dc.set_high()?;
            for chunk in data.chunks(4096) {
                self.spi.write(chunk)?;
            }
            Ok::<_, sys::EspError>(())
        })();
        let release = self.cs.set_high();
        result.and(release).map_err(|e| e.to_string())
    }
    fn delay_ms(&mut self, ms: u32) {
        delay(ms);
    }
    fn wait_idle(&mut self, timeout_ms: u32) -> Result<(), String> {
        let start = Instant::now();
        while self.busy.is_high() == self.busy_active_high {
            if start.elapsed() >= Duration::from_millis(timeout_ms.into()) {
                return Err("Panel BUSY timeout".into());
            }
            delay(1);
        }
        Ok(())
    }

    fn command_stream(
        &mut self,
        command: u8,
        mut data: impl Iterator<Item = u8>,
    ) -> Result<(), String> {
        self.cs.set_low().map_err(|e| e.to_string())?;
        let result = (|| {
            self.dc.set_low()?;
            self.spi.write(&[command])?;
            self.dc.set_high()?;
            let mut chunk = [0; 1024];
            loop {
                let mut count = 0;
                for byte in &mut chunk {
                    let Some(value) = data.next() else { break };
                    *byte = value;
                    count += 1;
                }
                if count == 0 {
                    break;
                }
                self.spi.write(&chunk[..count])?;
            }
            Ok::<_, sys::EspError>(())
        })();
        let release = self.cs.set_high();
        result.and(release).map_err(|e| e.to_string())
    }
}

pub struct Screen {
    pub tailnet: crate::tailnet_menu::Manager,
    pub catalog: crate::catalog::Jobs,
    pub storage: Option<crate::storage::Storage>,
    display: Display<PanelBus>,
    input: crate::input::InputTask,
    fast_count: u8,
    wifi: Option<crate::wifi::Station>,
    #[cfg(feature = "bluetooth")]
    bluetooth: Option<crate::bluetooth::Service>,
}

impl Screen {
    pub fn prepare_sleep(&mut self) -> Result<(), String> {
        #[cfg(feature = "bluetooth")]
        {
            if let Some(bluetooth) = &mut self.bluetooth {
                match bluetooth.stop_and_wait(Duration::from_secs(5)) {
                    Ok(true) => {}
                    Ok(false) => return Err("Bluetooth still stopping; retry sleep".into()),
                    Err(error) => {
                        log::warn!("Bluetooth shutdown fallback: {error}");
                    }
                }
            }
            drop(self.bluetooth.take());
        }
        if !self.tailnet.close_and_wait(Duration::from_secs(5)) {
            return Err("Tailnet still stopping; retry sleep".into());
        }
        if !self.catalog.cancel_and_wait(Duration::from_secs(5)) {
            return Err("Catalog request still stopping; retry sleep".into());
        }
        self.set_light(0, 0)?;
        self.display
            .sleep()
            .map_err(|e| format!("Panel sleep: {e:?}"))?;
        if let Some(wifi) = &mut self.wifi {
            wifi.stop()?;
        }
        Ok(())
    }
    pub fn poll_wifi(&mut self) {
        #[cfg(feature = "bluetooth")]
        if let Some(bluetooth) = &mut self.bluetooth {
            let finished = match bluetooth.poll() {
                Ok(finished) => finished,
                Err(error) => {
                    log::warn!("Bluetooth: {error}");
                    true
                }
            };
            if finished {
                self.bluetooth = None;
            }
        }
        if let Some(wifi) = &mut self.wifi {
            wifi.poll();
        }
        self.tailnet
            .set_local_address(self.wifi.as_ref().and_then(|wifi| wifi.address()));
    }
    pub fn wifi_label(&self) -> &'static str {
        self.wifi.as_ref().map_or("Wi-Fi off", |wifi| wifi.label())
    }
    pub fn battery(&self) -> Option<femto_picco_board::battery::Sample> {
        self.input.battery()
    }
    pub fn set_light(&self, brightness: u8, warmth: u8) -> Result<(), String> {
        self.input.set_light(brightness, warmth)
    }

    pub fn event(&self) -> Option<crate::input::Event> {
        self.input.next()
    }

    pub fn remote_turn(
        &mut self,
        state: &femto_reader::state::ReaderState,
        allow_turn: bool,
    ) -> Option<femto_picco_board::input::Turn> {
        #[cfg(feature = "bluetooth")]
        {
            let bluetooth = self.bluetooth.as_mut()?;
            match bluetooth.poll_input(state, allow_turn) {
                Ok(action) => action.map(|action| match action {
                    femto_reader::remote::PageTurn::Previous => {
                        femto_picco_board::input::Turn::Previous
                    }
                    femto_reader::remote::PageTurn::Next => femto_picco_board::input::Turn::Next,
                }),
                Err(error) => {
                    log::warn!("Bluetooth input: {error}");
                    self.bluetooth = None;
                    None
                }
            }
        }
        #[cfg(not(feature = "bluetooth"))]
        {
            let _ = (state, allow_turn);
            None
        }
    }

    pub fn present_oriented(
        &mut self,
        plane: &[u8],
        orientation: femto_picco_board::orientation::Orientation,
    ) -> Result<(), String> {
        self.input.set_view(orientation, true)?;
        self.fast_count = 5;
        self.present(plane)?;
        self.input.set_view(orientation, false)
    }

    pub fn present(&mut self, plane: &[u8]) -> Result<(), String> {
        if cfg!(feature = "grayscale") {
            if plane.len() != 2 * femto_picco_board::PLANE_BYTES {
                return Err("Invalid grayscale framebuffer".into());
            }
            let (msb, lsb) = plane.split_at(femto_picco_board::PLANE_BYTES);
            if let Err(error) = self.display.grayscale(msb, lsb) {
                log::warn!("Grayscale failed, retrying after reset: {error:?}");
                self.display.initialize().map_err(|e| format!("{e:?}"))?;
                self.display
                    .grayscale(msb, lsb)
                    .map_err(|e| format!("{e:?}"))?;
            }
            self.fast_count = 5;
            return Ok(());
        }
        // Provisional cleanup interval; requires physical ghosting comparisons.
        let requested = if self.fast_count >= 5 {
            Refresh::Full
        } else {
            Refresh::Fast
        };
        let mode = match self.display.refresh(plane, requested) {
            Ok(mode) => mode,
            Err(error) => {
                log::warn!("Display failed, retrying with reset and full refresh: {error:?}");
                self.display.initialize().map_err(|e| format!("{e:?}"))?;
                self.display
                    .refresh(plane, Refresh::Full)
                    .map_err(|e| format!("{e:?}"))?
            }
        };
        self.fast_count = if mode == Refresh::Full {
            0
        } else {
            self.fast_count + 1
        };
        Ok(())
    }
}

pub fn open() -> Result<Screen, String> {
    let panel = probe()?;
    log::info!("Detected {panel:?}");
    let p = Peripherals::take().map_err(|e| e.to_string())?;
    let storage = match crate::storage::mount(
        p.sdmmc1,
        p.pins.gpio42,
        p.pins.gpio41,
        p.pins.gpio40,
        p.pins.gpio39,
        p.pins.gpio44,
        p.pins.gpio43,
        p.pins.gpio38,
    ) {
        Ok(storage) => {
            log::info!("SD card mounted at /sdcard");
            Some(storage)
        }
        Err(error) => {
            log::warn!("SD mount unavailable: {error}");
            None
        }
    };
    let spi = SpiDeviceDriver::new_single(
        p.spi2,
        p.pins.gpio48,
        p.pins.gpio47,
        None::<AnyInputPin>,
        None::<AnyOutputPin>,
        &config::DriverConfig::new(),
        &config::Config::new().baudrate(Hertz(20_000_000)),
    )
    .map_err(|e| e.to_string())?;
    let mut cs = PinDriver::output(p.pins.gpio18).map_err(|e| e.to_string())?;
    cs.set_high().map_err(|e| e.to_string())?;
    let busy = PinDriver::input(p.pins.gpio15, Pull::Up).map_err(|e| e.to_string())?;
    let bus = PanelBus {
        spi,
        cs,
        busy,
        busy_active_high: panel != Panel::Se0400nqw47,
        dc: PinDriver::output(p.pins.gpio17).map_err(|e| e.to_string())?,
        reset: PinDriver::output(p.pins.gpio16).map_err(|e| e.to_string())?,
    };
    let mut display = Display::new(bus, panel).map_err(|e| format!("{e:?}"))?;
    display.initialize().map_err(|e| format!("{e:?}"))?;
    let touch = (|| -> Result<_, String> {
        let bus = I2cDriver::new(
            p.i2c0,
            p.pins.gpio7,
            p.pins.gpio8,
            &I2cConfig::new().baudrate(Hertz(400_000)),
        )
        .map_err(|e| e.to_string())?;
        let irq = PinDriver::input(p.pins.gpio11, Pull::Up).map_err(|e| e.to_string())?;
        let reset = PinDriver::output(p.pins.gpio10).map_err(|e| e.to_string())?;
        crate::input::TouchDevice::new(bus, irq, reset)
    })();
    let touch = match touch {
        Ok(touch) => Some(touch),
        Err(error) => {
            log::warn!("Touch unavailable: {error}");
            None
        }
    };
    let mut battery_adc = match AdcDriver::new(p.adc1).and_then(|adc| {
        AdcChannelDriver::new(
            adc,
            p.pins.gpio1,
            &AdcChannelConfig {
                attenuation: DB_12,
                calibration: Calibration::Curve,
                ..Default::default()
            },
        )
    }) {
        Ok(channel) => Some(channel),
        Err(error) => {
            log::warn!("Calibrated battery ADC unavailable: {error}");
            None
        }
    };
    let input = crate::input::InputTask::start(
        PinDriver::input(p.pins.gpio21, Pull::Up).map_err(|e| e.to_string())?,
        PinDriver::input(p.pins.gpio9, Pull::Up).map_err(|e| e.to_string())?,
        PinDriver::input(p.pins.gpio2, Pull::Up).map_err(|e| e.to_string())?,
        touch,
        move || {
            let adc = battery_adc.as_mut()?;
            Some([adc.read().ok()?, adc.read().ok()?, adc.read().ok()?])
        },
    )?;
    #[cfg(feature = "bluetooth")]
    let (wifi_modem, bluetooth_modem) = p.modem.split();
    #[cfg(not(feature = "bluetooth"))]
    let wifi_modem = p.modem;
    #[cfg(feature = "bluetooth")]
    let bluetooth = match crate::bluetooth::Service::open(bluetooth_modem) {
        Ok(scanner) => scanner,
        Err(error) => {
            log::warn!("Bluetooth unavailable: {error}");
            None
        }
    };
    let wifi = match crate::wifi::Station::open(wifi_modem) {
        Ok(wifi) => wifi,
        Err(error) => {
            log::warn!("Wi-Fi unavailable: {error}");
            None
        }
    };
    Ok(Screen {
        tailnet: Default::default(),
        catalog: femto_catalog::worker::Worker::default(),
        wifi,
        #[cfg(feature = "bluetooth")]
        bluetooth,
        display,
        storage,
        input,
        fast_count: 5,
    })
}
