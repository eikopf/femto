use crate::light_command::LightCommand;
use esp_idf_svc::hal::{
    delay::TickType,
    gpio::{Input, Output, PinDriver},
    i2c::I2cDriver,
};
use femto_picco_board::{
    input::{Buttons, Press, Turn, ViewGate},
    orientation::Orientation,
    touch::{Gesture, MAX_REPORT, Touch},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub enum Event {
    /// The power button: sleep while reading, select inside menus.
    Power,
    /// Opens the menu or activates its selection.
    Select,
    Back,
    /// A page button, named for its turn in the lower-forward arrangement.
    Button(Turn),
    Swipe(Turn),
    /// A turn that is already in reading direction.
    Turn(Turn),
    Tap {
        x: u16,
        y: u16,
    },
}

pub struct TouchDevice {
    bus: I2cDriver<'static>,
    irq: PinDriver<'static, Input>,
    reset: PinDriver<'static, Output>,
    gestures: Touch,
    enabled: bool,
    frontlight: femto_picco_board::frontlight::Frontlight,
    charger: Option<femto_picco_board::battery::Charger>,
}

impl TouchDevice {
    pub fn new(
        bus: I2cDriver<'static>,
        irq: PinDriver<'static, Input>,
        reset: PinDriver<'static, Output>,
    ) -> Result<Self, String> {
        let mut device = Self {
            bus,
            irq,
            reset,
            gestures: Touch::default(),
            enabled: true,
            frontlight: Default::default(),
            charger: None,
        };
        if let Err(error) = device.initialize() {
            log::warn!("Touch disabled during initialization: {error}");
            device.enabled = false;
        }
        device.charger =
            femto_picco_board::battery::Charger::detect(&mut ChargerRegisters(&mut device.bus));
        log::info!(
            "Touch ready={}; charger={:?}",
            device.enabled,
            device.charger
        );
        Ok(device)
    }

    fn initialize(&mut self) -> Result<(), String> {
        for (high, ms) in [(true, 1), (false, 10), (true, 50)] {
            if high {
                self.reset.set_high()
            } else {
                self.reset.set_low()
            }
            .map_err(|e| e.to_string())?;
            thread::sleep(Duration::from_millis(ms));
        }
        let mut report = [0; MAX_REPORT];
        for _ in 0..4 {
            if self.irq.is_high() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
            self.read_report(&mut report)?;
        }
        let mut mode = self.mode(&mut report)?;
        if mode == 0xff {
            // Launch the installed application, without writing touch firmware.
            let mut packet = [4, 0, 11, 0, 0x40, 0, 1, 0x3b, 0, 0, 0, 0, 0x17];
            let mut crc = 0xffff_u16;
            for byte in &packet[6..10] {
                crc ^= u16::from(*byte) << 8;
                for _ in 0..8 {
                    crc = if crc & 0x8000 != 0 {
                        (crc << 1) ^ 0x1021
                    } else {
                        crc << 1
                    };
                }
            }
            packet[10..12].copy_from_slice(&crc.to_le_bytes());
            self.write(&packet)?;
            self.wait_irq()?;
            self.read_report(&mut report)?;
            thread::sleep(Duration::from_millis(50));
            mode = self.mode(&mut report)?;
        }
        if mode != 0xf7 {
            return Err(format!("Touch application mode not confirmed: {mode:02x}"));
        }
        Ok(())
    }

    fn write(&mut self, data: &[u8]) -> Result<(), String> {
        self.bus
            .write(0x24, data, TickType::new_millis(20).ticks())
            .map_err(|e| e.to_string())
    }
    fn wait_irq(&self) -> Result<(), String> {
        let start = Instant::now();
        while self.irq.is_high() {
            if start.elapsed() >= Duration::from_millis(500) {
                return Err("Touch IRQ timeout".into());
            }
            thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }
    fn mode(&mut self, report: &mut [u8; MAX_REPORT]) -> Result<u8, String> {
        for _ in 0..3 {
            self.write(&[1, 0])?;
            self.wait_irq()?;
            let len = self.read_report(report)?;
            if len >= 3 && matches!(report[2], 0xf7 | 0xff) {
                return Ok(report[2]);
            }
        }
        Err("Touch descriptor unavailable".into())
    }
    fn read_report(&mut self, report: &mut [u8; MAX_REPORT]) -> Result<usize, String> {
        let mut header = [0; 2];
        let timeout = TickType::new_millis(20).ticks();
        self.bus
            .read(0x24, &mut header, timeout)
            .map_err(|e| e.to_string())?;
        let len = usize::from(u16::from_le_bytes(header));
        if len == 0 {
            return Ok(0);
        }
        if !(2..=MAX_REPORT).contains(&len) {
            return Err("Invalid touch report length".into());
        }
        self.bus
            .read(0x24, &mut report[..len], timeout)
            .map_err(|e| e.to_string())?;
        if report[..2] != header {
            return Err("Touch report length changed".into());
        }
        Ok(len)
    }
    fn poll(&mut self, now: u64) -> Result<Option<Event>, String> {
        self.gestures.expire(now);
        if !self.enabled || self.irq.is_high() {
            return Ok(None);
        }
        let mut report = [0; MAX_REPORT];
        let len = self.read_report(&mut report)?;
        if len < 3 {
            self.gestures.cancel();
            return Ok(None);
        }
        Ok(self
            .gestures
            .report(&report[..len], now)
            .map(|gesture| match gesture {
                Gesture::Tap { x, y } => Event::Tap { x, y },
                Gesture::Swipe(turn) => Event::Swipe(turn),
            }))
    }
}

struct LightRegisters<'a>(&'a mut I2cDriver<'static>);
struct ChargerRegisters<'a>(&'a mut I2cDriver<'static>);

impl femto_picco_board::battery::Registers for ChargerRegisters<'_> {
    type Error = String;
    fn read(&mut self, address: u8, register: u8) -> Result<u8, String> {
        read_register(self.0, address, register)
    }
}

fn read_register(bus: &mut I2cDriver<'static>, address: u8, register: u8) -> Result<u8, String> {
    let mut value = [0];
    let timeout = TickType::new_millis(50).ticks();
    bus.write_read(address, &[register], &mut value, timeout)
        .map_err(|error| format!("I2C address={address:#04x} register={register:#04x}: {error}"))?;
    Ok(value[0])
}

impl femto_picco_board::frontlight::Registers for LightRegisters<'_> {
    type Error = String;
    fn read(&mut self, register: u8) -> Result<u8, String> {
        read_register(self.0, 0x38, register)
    }
    fn write(&mut self, register: u8, value: u8) -> Result<(), String> {
        self.0
            .write(0x38, &[register, value], TickType::new_millis(20).ticks())
            .map_err(|e| format!("frontlight write register {register:#04x}: {e}"))
    }
    fn delay_ms(&mut self, ms: u32) {
        thread::sleep(Duration::from_millis(ms.into()));
    }
}
enum Command {
    Light(LightCommand),
    View {
        generation: u64,
        orientation: Orientation,
        paused: bool,
        reply: SyncSender<()>,
    },
}

pub struct InputTask {
    events: Receiver<(u64, Instant, Event)>,
    battery: Arc<Mutex<Option<(Instant, femto_picco_board::battery::Sample)>>>,
    commands: SyncSender<Command>,
    stop: Arc<AtomicBool>,
    long_presses: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    generation: u64,
}

impl InputTask {
    pub fn start(
        previous: PinDriver<'static, Input>,
        next: PinDriver<'static, Input>,
        select: PinDriver<'static, Input>,
        mut touch: Option<TouchDevice>,
        mut read_battery: impl FnMut() -> Option<[u16; 3]> + Send + 'static,
    ) -> Result<Self, String> {
        let (sender, events) = mpsc::sync_channel(8);
        let (commands, requests) = mpsc::sync_channel::<Command>(4);
        let battery = Arc::new(Mutex::new(None));
        let battery_sample = battery.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let long_presses = Arc::new(AtomicBool::new(false));
        let long = long_presses.clone();
        let worker = thread::Builder::new()
            .name("femto-input".into())
            .stack_size(8192)
            .spawn(move || {
                let started = Instant::now();
                let mut buttons = Buttons::default();
                let mut power_button = Buttons::default();
                let mut touch_errors = 0;
                let mut battery_at: Option<Instant> = None;
                let mut view = ViewGate::default();
                while !stopped.load(Ordering::Relaxed) {
                    if battery_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(30)) {
                        let adc_mv = read_battery();
                        let charger = touch.as_mut().and_then(|device| {
                            device
                                .charger?
                                .status(&mut ChargerRegisters(&mut device.bus))
                                .inspect_err(|error| log::warn!("Charger status read: {error}"))
                                .ok()
                        });
                        let now = Instant::now();
                        if let Ok(mut slot) = battery_sample.lock() {
                            *slot = Some((
                                now,
                                femto_picco_board::battery::Sample::from_readings(adc_mv, charger),
                            ));
                        }
                        battery_at = Some(now);
                    }
                    if let Ok(command) = requests.try_recv() {
                        match command {
                            Command::View {
                                generation: next,
                                orientation,
                                paused: pause,
                                reply,
                            } => {
                                if !view.change(next, pause) {
                                    break;
                                }
                                if let Some(device) = &mut touch {
                                    device.gestures.set_orientation(orientation);
                                    device.gestures.cancel();
                                }
                                let _ = reply.try_send(());
                            }
                            Command::Light(command) => {
                                command.execute(Instant::now(), |brightness, warmth| {
                                    match &mut touch {
                                        Some(device) => device
                                            .frontlight
                                            .apply(
                                                &mut LightRegisters(&mut device.bus),
                                                brightness,
                                                warmth,
                                            )
                                            .map_err(|e| format!("Frontlight: {e:?}")),
                                        None => Err("Frontlight I2C unavailable".into()),
                                    }
                                });
                            }
                        }
                    }
                    let now = started.elapsed().as_millis() as u64;
                    let buttons_generation =
                        view.buttons(previous.is_high() && next.is_high() && select.is_high());
                    let send = |event| {
                        if let Some(generation) = buttons_generation {
                            let _ = sender.try_send((generation, Instant::now(), event));
                        }
                    };
                    if power_button
                        .sample(select.is_low(), false, now, false)
                        .is_some()
                    {
                        send(Event::Power);
                    }
                    match buttons.sample(
                        previous.is_low(),
                        next.is_low(),
                        now,
                        long.load(Ordering::Relaxed),
                    ) {
                        Some(Press::Short(turn)) => send(Event::Button(turn)),
                        Some(Press::Long(_)) => send(Event::Back),
                        None => {}
                    }
                    if let Some(device) = &mut touch {
                        match device.poll(now) {
                            Ok(event) => {
                                touch_errors = 0;
                                if let Some(event) = event {
                                    if let Some(generation) = view.touch() {
                                        let _ =
                                            sender.try_send((generation, Instant::now(), event));
                                    }
                                }
                            }
                            Err(error) => {
                                device.gestures.cancel();
                                touch_errors += 1;
                                if touch_errors >= 3 {
                                    log::error!("Touch disabled after repeated errors: {error}");
                                    device.enabled = false;
                                }
                            }
                        }
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            events,
            battery,
            commands,
            stop,
            long_presses,
            worker: Some(worker),
            generation: 0,
        })
    }
    /// Long page-button presses mean back, which delays short presses until
    /// release; reading keeps them immediate.
    pub fn set_long_presses(&self, enabled: bool) {
        self.long_presses.store(enabled, Ordering::Relaxed);
    }
    pub fn battery(&self) -> Option<femto_picco_board::battery::Sample> {
        self.battery
            .lock()
            .ok()?
            .as_ref()
            .filter(|(at, _)| at.elapsed() <= Duration::from_secs(60))
            .map(|(_, sample)| *sample)
    }
    pub fn set_light(&self, brightness: u8, warmth: u8) -> Result<(), String> {
        let (command, reply) = LightCommand::new(brightness, warmth);
        self.commands
            .try_send(Command::Light(command))
            .map_err(|e| format!("Frontlight command unavailable: {e}"))?;
        reply.wait()
    }
    pub fn set_view(&mut self, orientation: Orientation, paused: bool) -> Result<(), String> {
        let generation = self
            .generation
            .checked_add(1)
            .ok_or("Input generation exhausted")?;
        let (reply, result) = mpsc::sync_channel(1);
        self.commands
            .try_send(Command::View {
                generation,
                orientation,
                paused,
                reply,
            })
            .map_err(|_| "Input view command unavailable".to_string())?;
        result
            .recv_timeout(Duration::from_secs(1))
            .map_err(|_| "Input view result unknown: stop rendering".to_string())?;
        self.generation = generation;
        Ok(())
    }
    pub fn next(&self) -> Option<Event> {
        while let Ok((generation, at, event)) = self.events.try_recv() {
            if generation == self.generation && at.elapsed() <= Duration::from_secs(3) {
                return Some(event);
            }
        }
        None
    }
}

impl Drop for InputTask {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
