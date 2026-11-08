//! Read-only Picco battery interpretation from calibrated ADC samples and charger status.
//! Voltage tables follow the pinned FreeInk reference; see THIRD-PARTY.md.
pub const CHARGER_ADDRESS: u8 = 0x03;
pub const STATUS_REGISTER: u8 = 0x08;
const CHARGING: [u16; 21] = [
    4305, 4305, 4300, 4290, 4235, 4187, 4124, 4078, 4045, 4013, 3984, 3961, 3941, 3923, 3906, 3894,
    3876, 3840, 3805, 3760, 3500,
];
const DISCHARGING: [u16; 21] = [
    4305, 4229, 4171, 4118, 4065, 4019, 3955, 3915, 3876, 3841, 3812, 3786, 3764, 3745, 3730, 3717,
    3697, 3667, 3636, 3565, 3300,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    Battery,
    Charging,
    External,
    Complete,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub millivolts: Option<u16>,
    pub power: Option<Power>,
    pub percentage: Option<u8>,
}
impl Sample {
    /// Input voltages are calibrated millivolts at the ADC pin, before the 2:1 divider.
    /// Charger status must come from one register transaction, or be absent on failure.
    pub fn from_readings(adc_mv: Option<[u16; 3]>, charger: Option<u8>) -> Self {
        let millivolts = adc_mv.and_then(|mut samples| {
            samples.sort_unstable();
            samples[1]
                .checked_mul(2)
                .filter(|mv| (2500..=4500).contains(mv))
        });
        let power = charger.map(|status| {
            if status & 2 == 0 {
                Power::Battery
            } else {
                match (status >> 3) & 3 {
                    1 | 2 => Power::Charging,
                    3 => Power::Complete,
                    _ => Power::External,
                }
            }
        });
        let percentage = millivolts
            .zip(power)
            .map(|(mv, power)| percentage(mv, power == Power::Charging));
        Self {
            millivolts,
            power,
            percentage,
        }
    }
}
fn percentage(mv: u16, charging: bool) -> u8 {
    let table = if charging { &CHARGING } else { &DISCHARGING };
    if mv <= table[20] {
        return 0;
    }
    if mv >= table[0] {
        return 100;
    }
    for i in 1..=20 {
        if mv < table[i] {
            continue;
        }
        let hi = table[i - 1];
        let lo = table[i];
        let span = if hi == table[0] { 5 * i as u32 } else { 5 };
        let interpolation = if hi > lo {
            span * u32::from(mv - lo) / u32::from(hi - lo)
        } else {
            span
        };
        return (100 - 5 * i as u32 + interpolation) as u8;
    }
    0
}
