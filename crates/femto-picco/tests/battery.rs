use femto_picco_board::battery::{Power, Sample};

#[test]
fn charger_identity_selects_only_supported_addresses() {
    use femto_picco_board::battery::{Charger, Registers};
    struct Bus {
        aw: Result<u8, ()>,
        sgm: Result<u8, ()>,
        status_address: Option<u8>,
    }
    impl Registers for Bus {
        type Error = ();
        fn read(&mut self, address: u8, register: u8) -> Result<u8, ()> {
            match (address, register) {
                (0x49, 0x0a) => self.aw,
                (0x03, 0x0b) => self.sgm,
                (address, 0x08) if Some(address) == self.status_address => Ok(0x7a),
                _ => Err(()),
            }
        }
    }
    for (aw, sgm, expected) in [
        (Ok(0x49), Err(()), Some(Charger::Aw32001)),
        (Ok(0x49), Ok(1), Some(Charger::Aw32001)),
        (Err(()), Ok(1), Some(Charger::Sgm41562)),
        (Ok(0xff), Ok(2), Some(Charger::Sgm41562)),
        (Err(()), Err(()), None),
        (Ok(0), Ok(0), None),
        (Ok(0xff), Ok(0xff), None),
    ] {
        let mut bus = Bus {
            aw,
            sgm,
            status_address: expected.map(Charger::address),
        };
        assert_eq!(Charger::detect(&mut bus), expected);
        if let Some(charger) = expected {
            let status = charger.status(&mut bus).unwrap();
            assert_eq!(
                Sample::from_readings(Some([2160; 3]), Some(status)).power,
                Some(Power::Complete)
            );
            bus.status_address = None;
            assert!(charger.status(&mut bus).is_err());
        }
    }
}
#[test]
fn charger_state_is_decoded_from_one_status_byte() {
    for raw in 0..=255 {
        let sample = Sample::from_readings(Some([2000; 3]), Some(raw));
        let expected = if raw & 2 == 0 {
            Power::Battery
        } else {
            match (raw >> 3) & 3 {
                1 | 2 => Power::Charging,
                3 => Power::Complete,
                _ => Power::External,
            }
        };
        assert_eq!(sample.power, Some(expected));
        assert!(sample.percentage.is_some());
    }
}
#[test]
fn missing_or_invalid_measurements_remain_unknown() {
    assert_eq!(
        Sample::from_readings(None, None),
        Sample {
            millivolts: None,
            power: None,
            percentage: None
        }
    );
    for samples in [[0; 3], [1249; 3], [2251; 3], [u16::MAX; 3]] {
        let sample = Sample::from_readings(Some(samples), Some(0));
        assert_eq!(sample.millivolts, None);
        assert_eq!(sample.percentage, None);
        assert_eq!(sample.power, Some(Power::Battery));
    }
    let sample = Sample::from_readings(Some([2000; 3]), None);
    assert_eq!(sample.millivolts, Some(4000));
    assert_eq!(sample.percentage, None);
}
#[test]
fn divider_median_and_reference_curves_handle_noise_and_duplicate_top_anchor() {
    for samples in [[1800, 1900, 2200], [2200, 1800, 1900], [1900, 2200, 1800]] {
        assert_eq!(
            Sample::from_readings(Some(samples), Some(0)).millivolts,
            Some(3800)
        );
    }
    for status in [0, 0x0a] {
        let mut previous = 0;
        for pin_mv in 1250..=2250 {
            let current = Sample::from_readings(Some([pin_mv; 3]), Some(status))
                .percentage
                .unwrap();
            assert!(current >= previous && current <= 100);
            previous = current;
        }
    }
    assert_eq!(
        Sample::from_readings(Some([2150; 3]), Some(0x0a)).percentage,
        Some(90)
    );
    assert_eq!(
        Sample::from_readings(Some([2151; 3]), Some(0x0a)).percentage,
        Some(94)
    );
    assert_eq!(
        Sample::from_readings(Some([2152; 3]), Some(0x0a)).percentage,
        Some(98)
    );
    assert_eq!(
        Sample::from_readings(Some([2153; 3]), Some(0x0a)).percentage,
        Some(100)
    );
    assert_eq!(
        Sample::from_readings(Some([1906; 3]), Some(0)).percentage,
        Some(50)
    );
    assert_eq!(
        Sample::from_readings(Some([1650; 3]), Some(0)).percentage,
        Some(0)
    );
    // Completion is charger telemetry, not a reason to override a low measured voltage.
    assert_eq!(
        Sample::from_readings(Some([1650; 3]), Some(0x1a)).percentage,
        Some(0)
    );
}
