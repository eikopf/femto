use femto_picco_board::battery::{Power, Sample};
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
