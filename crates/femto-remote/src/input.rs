use hidreport::{Field, FieldAttributes, Report, ReportDescriptor, Usage};

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Presses {
    usages: [u32; 32],
    len: usize,
}

impl Presses {
    pub fn as_slice(&self) -> &[u32] {
        &self.usages[..self.len]
    }

    fn insert(&mut self, usage: u32) -> Result<(), &'static str> {
        if !self.as_slice().contains(&usage) {
            if self.len == self.usages.len() {
                return Err("pressed usage limit");
            }
            self.usages[self.len] = usage;
            self.len += 1;
        }
        Ok(())
    }
}

pub struct RemoteInput {
    descriptor: ReportDescriptor,
    reports: [(u8, Presses); 8],
    count: usize,
}

fn supported(usage: Usage) -> bool {
    matches!(u16::from(usage.usage_page), 7 | 12)
}

fn add_pressed(pressed: &mut Presses, usage: Usage) -> Result<(), &'static str> {
    if !supported(usage) || u16::from(usage.usage_id) == 0 {
        return Ok(());
    }
    if u16::from(usage.usage_page) == 7 && u16::from(usage.usage_id) <= 3 {
        return Err("keyboard rollover or error");
    }
    pressed.insert(u32::from(usage))
}

impl RemoteInput {
    pub(crate) fn report_ids(&self) -> impl Iterator<Item = u8> + '_ {
        self.reports[..self.count].iter().map(|(id, _)| *id)
    }

    pub fn new(bytes: &[u8]) -> Result<Self, &'static str> {
        crate::preflight(bytes)?;
        let descriptor = ReportDescriptor::try_from(bytes).map_err(|_| "descriptor parse")?;
        let mut reports = [(0, Presses::default()); 8];
        let count = descriptor.input_reports().len();
        if count == 0 || count > reports.len() {
            return Err("input report count");
        }
        for (slot, report) in reports.iter_mut().zip(descriptor.input_reports()) {
            slot.0 = report.report_id().as_ref().map(u8::from).unwrap_or(0);
        }
        Ok(Self {
            descriptor,
            reports,
            count,
        })
    }

    /// Clears held state after disconnect or input queue loss without events.
    pub fn reset(&mut self) {
        for (_, pressed) in &mut self.reports[..self.count] {
            *pressed = Presses::default();
        }
    }

    /// Returns new presses only; malformed input preserves all prior state.
    pub fn accept(&mut self, reference: &[u8], payload: &[u8]) -> Result<Presses, &'static str> {
        let frame = crate::frame_ble_input(&self.descriptor, reference, payload)?;
        let bytes = frame.as_bytes();
        let report = self
            .descriptor
            .find_input_report(bytes)
            .ok_or("input report")?;
        let mut pressed = Presses::default();
        for field in report.fields() {
            match field {
                Field::Variable(field) if supported(field.usage) => {
                    if field.is_relative()
                        || i32::from(field.logical_minimum) != 0
                        || i32::from(field.logical_maximum) != 1
                    {
                        return Err("non-button variable");
                    }
                    match u32::from(field.extract(bytes).map_err(|_| "variable extraction")?) {
                        0 => {}
                        1 => add_pressed(&mut pressed, field.usage)?,
                        _ => return Err("button value"),
                    }
                }
                Field::Array(field) if field.usages().iter().copied().any(supported) => {
                    let minimum = i32::from(field.logical_minimum);
                    let maximum = i32::from(field.logical_maximum);
                    if field.is_relative() || minimum < 0 || maximum < minimum {
                        return Err("array logical range");
                    }
                    for index in 0..usize::from(field.report_count) {
                        let value = u32::from(
                            field
                                .extract_one(bytes, index)
                                .map_err(|_| "array extraction")?,
                        );
                        if value < minimum as u32 || value > maximum as u32 {
                            if field.has_null_state() {
                                continue;
                            }
                            return Err("array value");
                        }
                        let Some(usage) = field.usages().get((value - minimum as u32) as usize)
                        else {
                            if field.has_null_state() {
                                continue;
                            }
                            return Err("array usage");
                        };
                        add_pressed(&mut pressed, *usage)?;
                    }
                }
                _ => {}
            }
        }
        let slot = self.reports[..self.count]
            .iter()
            .position(|(id, _)| *id == reference[0])
            .ok_or("report state")?;
        let mut before = Presses::default();
        let mut after = Presses::default();
        for (index, (_, previous)) in self.reports[..self.count].iter().enumerate() {
            for usage in previous.as_slice() {
                before.insert(*usage)?;
            }
            let current = if index == slot { &pressed } else { previous };
            for usage in current.as_slice() {
                after.insert(*usage)?;
            }
        }
        let mut events = Presses::default();
        for usage in after.as_slice() {
            if !before.as_slice().contains(usage) {
                events.insert(*usage)?;
            }
        }
        self.reports[slot].1 = pressed;
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ARRAY: &[u8] = &[
        0x05, 7, 0x19, 0, 0x29, 255, 0x15, 0, 0x26, 255, 0, 0x75, 8, 0x95, 6, 0x81, 0,
    ];

    #[test]
    fn held_duplicates_release_repress_and_disconnect() {
        let mut input = RemoteInput::new(ARRAY).unwrap();
        let report = [0x4e, 0x4e, 0, 0, 0, 0];
        assert_eq!(
            input.accept(&[0, 1], &report).unwrap().as_slice(),
            &[0x7004e]
        );
        assert!(
            input
                .accept(&[0, 1], &report)
                .unwrap()
                .as_slice()
                .is_empty()
        );
        assert!(
            input
                .accept(&[0, 1], &[0; 6])
                .unwrap()
                .as_slice()
                .is_empty()
        );
        assert_eq!(
            input.accept(&[0, 1], &report).unwrap().as_slice(),
            &[0x7004e]
        );
        input.reset();
        assert_eq!(
            input.accept(&[0, 1], &report).unwrap().as_slice(),
            &[0x7004e]
        );
    }

    #[test]
    fn malformed_and_rollover_reports_do_not_release_held_keys() {
        let mut input = RemoteInput::new(ARRAY).unwrap();
        let held = [0x4e, 0, 0, 0, 0, 0];
        input.accept(&[0, 1], &held).unwrap();
        for bad in [&[1, 1, 1, 1, 1, 1][..], &[0x4e], &[], &[0; 7]] {
            assert!(input.accept(&[0, 1], bad).is_err());
            assert!(input.accept(&[0, 1], &held).unwrap().as_slice().is_empty());
        }
    }

    #[test]
    fn shared_usage_across_report_ids_emits_one_press() {
        let mut bytes = vec![0x85, 1];
        bytes.extend_from_slice(ARRAY);
        bytes.extend_from_slice(&[0x85, 2]);
        bytes.extend_from_slice(ARRAY);
        let mut input = RemoteInput::new(&bytes).unwrap();
        let held = [0x4e, 0, 0, 0, 0, 0];
        assert_eq!(input.accept(&[1, 1], &held).unwrap().as_slice(), &[0x7004e]);
        assert!(input.accept(&[2, 1], &held).unwrap().as_slice().is_empty());
        input.accept(&[1, 1], &[0; 6]).unwrap();
        assert!(input.accept(&[1, 1], &held).unwrap().as_slice().is_empty());
        input.accept(&[1, 1], &[0; 6]).unwrap();
        input.accept(&[2, 1], &[0; 6]).unwrap();
        assert_eq!(input.accept(&[2, 1], &held).unwrap().as_slice(), &[0x7004e]);
    }

    #[test]
    fn consumer_button_transitions() {
        let bytes = [
            0x05, 12, 0x85, 2, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 2, 0x09, 0xe9, 0x09, 0xea, 0x81, 2,
            0x75, 6, 0x95, 1, 0x81, 3,
        ];
        let mut input = RemoteInput::new(&bytes).unwrap();
        assert_eq!(input.accept(&[2, 1], &[1]).unwrap().as_slice(), &[0xc00e9]);
        assert_eq!(input.accept(&[2, 1], &[3]).unwrap().as_slice(), &[0xc00ea]);
        assert!(input.accept(&[2, 1], &[3]).unwrap().as_slice().is_empty());
    }

    #[test]
    fn pressed_limit_rejects_entire_update_without_losing_state() {
        let mut bytes = ARRAY.to_vec();
        bytes[14] = 33;
        let mut input = RemoteInput::new(&bytes).unwrap();
        let mut held = [0; 33];
        for (index, value) in held[..32].iter_mut().enumerate() {
            *value = index as u8 + 4;
        }
        assert_eq!(input.accept(&[0, 1], &held).unwrap().as_slice().len(), 32);
        let mut overflow = held;
        overflow[32] = 36;
        assert!(input.accept(&[0, 1], &overflow).is_err());
        assert!(input.accept(&[0, 1], &held).unwrap().as_slice().is_empty());
    }

    #[test]
    fn pressed_limit_applies_across_report_ids() {
        let mut array = ARRAY.to_vec();
        array[14] = 20;
        let mut bytes = vec![0x85, 1];
        bytes.extend_from_slice(&array);
        bytes.extend_from_slice(&[0x85, 2]);
        bytes.extend_from_slice(&array);
        let mut input = RemoteInput::new(&bytes).unwrap();
        let first: Vec<u8> = (4..24).collect();
        let second: Vec<u8> = (24..44).collect();
        assert_eq!(input.accept(&[1, 1], &first).unwrap().as_slice().len(), 20);
        assert!(input.accept(&[2, 1], &second).is_err());
        assert!(input.accept(&[1, 1], &first).unwrap().as_slice().is_empty());
        assert!(input.accept(&[2, 1], &first).unwrap().as_slice().is_empty());
    }
}
