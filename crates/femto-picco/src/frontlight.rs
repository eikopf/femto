//! Picco dual-channel frontlight at I2C address 0x38.
mod table;

pub trait Registers {
    type Error;
    fn read(&mut self, register: u8) -> Result<u8, Self::Error>;
    fn write(&mut self, register: u8, value: u8) -> Result<(), Self::Error>;
    fn delay_ms(&mut self, ms: u32);
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    Range,
    Bus(E),
    Restore { cause: E, restore: E },
}

#[derive(Default)]
pub struct Frontlight {
    configured: bool,
    confirmed: Option<(u8, u8)>,
}

fn update<B: Registers>(
    bus: &mut B,
    register: u8,
    mask: u8,
    value: u8,
) -> Result<(), Error<B::Error>> {
    let current = bus.read(register).map_err(Error::Bus)?;
    bus.write(register, (current & !mask) | (value & mask))
        .map_err(Error::Bus)
}

impl Frontlight {
    /// Brightness is 0 (off) through 10; warmth is 0 (cool) through 9 (warm).
    /// A bus failure attempts the last successful setting once, if available.
    /// Successful restoration still returns the original Bus error; Restore
    /// reports both failures and discards the remembered setting.
    pub fn apply<B: Registers>(
        &mut self,
        bus: &mut B,
        brightness: u8,
        warmth: u8,
    ) -> Result<(), Error<B::Error>> {
        if brightness > 10 || warmth > 9 {
            return Err(Error::Range);
        }
        match self.apply_registers(bus, brightness, warmth) {
            Ok(()) => {
                self.confirmed = Some((brightness, warmth));
                Ok(())
            }
            Err(Error::Bus(cause)) => {
                if let Some((old_brightness, old_warmth)) = self.confirmed.take() {
                    match self.apply_registers(bus, old_brightness, old_warmth) {
                        Ok(()) => self.confirmed = Some((old_brightness, old_warmth)),
                        Err(Error::Bus(restore)) => return Err(Error::Restore { cause, restore }),
                        Err(error) => return Err(error),
                    }
                }
                Err(Error::Bus(cause))
            }
            Err(error) => Err(error),
        }
    }

    fn apply_registers<B: Registers>(
        &mut self,
        bus: &mut B,
        brightness: u8,
        warmth: u8,
    ) -> Result<(), Error<B::Error>> {
        let configured = self.configured;
        self.configured = false;
        if brightness == 0 {
            update(bus, 0, 0x14, 0)?;
            update(bus, 0, 0x0b, 0)?;
            return update(bus, 0, 0x80, 0x80);
        }
        if !configured {
            update(bus, 0, 0x80, 0)?;
            bus.delay_ms(2);
            bus.write(0x50, 3).map_err(Error::Bus)?;
            update(bus, 1, 7, 0)?;
            bus.write(2, 0x38).map_err(Error::Bus)?;
        }
        let [cool, warm, cool_current, warm_current] =
            table::LEVELS[usize::from(warmth)][usize::from(brightness - 1)];
        bus.write(3, cool).map_err(Error::Bus)?;
        bus.write(4, warm).map_err(Error::Bus)?;
        update(bus, 5, 0x1f, cool_current)?;
        update(bus, 6, 0x1f, warm_current)?;
        update(bus, 0, 0x14, if cool != 0 { 4 } else { 0 })?;
        update(bus, 0, 0x0b, if warm != 0 { 2 } else { 0 })?;
        self.configured = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Bus {
        registers: [u8; 256],
        operations: usize,
        fail: Option<usize>,
        fail_again: Option<usize>,
        wakes: usize,
    }
    impl Default for Bus {
        fn default() -> Self {
            Self {
                registers: [0xe0; 256],
                operations: 0,
                fail: None,
                fail_again: None,
                wakes: 0,
            }
        }
    }
    impl Bus {
        fn step(&mut self) -> Result<(), ()> {
            self.operations += 1;
            if self.fail == Some(self.operations) || self.fail_again == Some(self.operations) {
                Err(())
            } else {
                Ok(())
            }
        }
    }
    impl Registers for Bus {
        type Error = ();
        fn read(&mut self, r: u8) -> Result<u8, ()> {
            self.step()?;
            Ok(self.registers[r as usize])
        }
        fn write(&mut self, r: u8, v: u8) -> Result<(), ()> {
            self.step()?;
            self.registers[r as usize] = v;
            Ok(())
        }
        fn delay_ms(&mut self, ms: u32) {
            assert_eq!(ms, 2);
            self.wakes += 1;
        }
    }
    #[test]
    fn levels_and_masked_registers() {
        for warmth in 0..=9 {
            for brightness in 1..=10 {
                let mut bus = Bus::default();
                let mut light = Frontlight::default();
                light.apply(&mut bus, brightness, warmth).unwrap();
                assert!(bus.registers[3] <= 177);
                assert!(bus.registers[4] <= 184);
                assert_eq!(bus.registers[5] & 0xe0, 0xe0);
                assert_eq!(bus.registers[6] & 0xe0, 0xe0);
                assert_eq!(bus.registers[0] & 0xe0, 0x60);
                light.apply(&mut bus, brightness, warmth).unwrap();
                assert_eq!(bus.wakes, 1);
                light.apply(&mut bus, 0, warmth).unwrap();
                assert_eq!(bus.registers[0] & 0x9f, 0x80);
                light.apply(&mut bus, brightness, warmth).unwrap();
                assert_eq!(bus.wakes, 2);
            }
        }
        assert_eq!(table::LEVELS[0][0], [1, 0, 24, 0]);
        assert_eq!(table::LEVELS[0][9], [177, 0, 31, 0]);
        assert_eq!(table::LEVELS[9][0], [0, 5, 0, 31]);
        assert_eq!(table::LEVELS[9][9], [0, 184, 0, 31]);
    }
    #[test]
    fn faults_require_reconfiguration_and_invalid_ranges_do_not_write() {
        let mut reference = Bus::default();
        Frontlight::default().apply(&mut reference, 5, 5).unwrap();
        for failed_operation in 1..=reference.operations {
            let mut bus = Bus {
                fail: Some(failed_operation),
                ..Default::default()
            };
            let mut light = Frontlight::default();
            assert_eq!(light.apply(&mut bus, 5, 5), Err(Error::Bus(())));
            let wakes = bus.wakes;
            bus.fail = None;
            light.apply(&mut bus, 5, 5).unwrap();
            assert_eq!(bus.wakes, wakes + 1);
        }
        let mut bus = Bus::default();
        let mut light = Frontlight::default();
        assert_eq!(light.apply(&mut bus, 11, 0), Err(Error::Range));
        assert_eq!(light.apply(&mut bus, 0, 10), Err(Error::Range));
        assert_eq!(bus.operations, 0);
    }

    #[test]
    fn partial_updates_restore_the_last_successful_setting_once() {
        for (before, next) in [((5, 5), (8, 1)), ((5, 5), (0, 5)), ((0, 5), (8, 1))] {
            let mut reference = Bus::default();
            let mut light = Frontlight::default();
            light.apply(&mut reference, before.0, before.1).unwrap();
            reference.operations = 0;
            light.apply(&mut reference, next.0, next.1).unwrap();
            for failed_operation in 1..=reference.operations {
                let mut bus = Bus::default();
                let mut light = Frontlight::default();
                light.apply(&mut bus, before.0, before.1).unwrap();
                let registers = bus.registers;
                bus.operations = 0;
                bus.fail = Some(failed_operation);
                assert_eq!(light.apply(&mut bus, next.0, next.1), Err(Error::Bus(())));
                assert_eq!(light.confirmed, Some(before));
                if before.0 == 0 {
                    assert_eq!(bus.registers[0] & 0x9f, 0x80);
                } else {
                    for register in [0, 3, 4, 5, 6] {
                        assert_eq!(bus.registers[register], registers[register]);
                    }
                }
                assert!(bus.operations <= failed_operation + 16);
                bus.fail = None;
                light.apply(&mut bus, next.0, next.1).unwrap();
                assert_eq!(light.confirmed, Some(next));
            }
        }
    }

    #[test]
    fn failed_restoration_discards_assumed_state_and_next_request_reconfigures() {
        let mut reference = Bus::default();
        Frontlight::default().apply(&mut reference, 5, 5).unwrap();
        for failed_restore in 1..=reference.operations {
            let mut bus = Bus::default();
            let mut light = Frontlight::default();
            light.apply(&mut bus, 5, 5).unwrap();
            bus.operations = 0;
            bus.fail = Some(2);
            bus.fail_again = Some(2 + failed_restore);
            assert_eq!(
                light.apply(&mut bus, 8, 1),
                Err(Error::Restore {
                    cause: (),
                    restore: ()
                })
            );
            assert_eq!(bus.operations, 2 + failed_restore);
            assert_eq!(light.confirmed, None);
            assert!(!light.configured);
            let wakes = bus.wakes;
            bus.fail = None;
            bus.fail_again = None;
            light.apply(&mut bus, 8, 1).unwrap();
            assert_eq!(bus.wakes, wakes + 1);
            assert_eq!(light.confirmed, Some((8, 1)));
            let before = bus.operations;
            assert_eq!(light.apply(&mut bus, 11, 1), Err(Error::Range));
            assert_eq!(bus.operations, before);
            assert_eq!(light.confirmed, Some((8, 1)));
        }
    }
}
