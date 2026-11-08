#[derive(Clone, Copy, Default)]
struct Globals {
    size: u32,
    count: u32,
    id: u32,
    has_usage_page: bool,
}

pub(crate) struct InputFrame {
    bytes: [u8; 65],
    len: usize,
}

impl InputFrame {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// Converts a BLE Report Reference and notification into parser framing.
pub(crate) fn frame_ble_input(
    parsed: &hidreport::ReportDescriptor,
    reference: &[u8],
    payload: &[u8],
) -> Result<InputFrame, &'static str> {
    use hidreport::Report;
    if reference.len() != 2 || reference[1] != 1 {
        return Err("input report reference");
    }
    if payload.is_empty() || payload.len() > 64 {
        return Err("notification bytes");
    }
    let id = reference[0];
    if !parsed
        .input_reports()
        .iter()
        .any(|report| report.report_id().as_ref().map(u8::from).unwrap_or(0) == id)
    {
        return Err("unknown report reference");
    }
    let mut frame = InputFrame {
        bytes: [0; 65],
        len: payload.len() + usize::from(id != 0),
    };
    let start = usize::from(id != 0);
    if id != 0 {
        frame.bytes[0] = id;
    }
    frame.bytes[start..frame.len].copy_from_slice(payload);
    validate_input(parsed, frame.as_bytes())?;
    Ok(frame)
}

/// Checks complete transport framing before any upstream field extraction.
pub(crate) fn validate_input(
    parsed: &hidreport::ReportDescriptor,
    bytes: &[u8],
) -> Result<(), &'static str> {
    use hidreport::Report;
    if bytes.is_empty() || bytes.len() > 65 {
        return Err("input bytes");
    }
    let report = parsed.find_input_report(bytes).ok_or("unknown report ID")?;
    if bytes.len() != report.size_in_bytes() {
        return Err("input length");
    }
    Ok(())
}

/// Rejects unsupported items and bounds parser work before allocating fields.
pub(crate) fn preflight(bytes: &[u8]) -> Result<(), &'static str> {
    if bytes.is_empty() || bytes.len() > 4096 {
        return Err("descriptor bytes");
    }
    let mut state = Globals::default();
    let mut stack = [Globals::default(); 8];
    let mut depth = 0;
    let mut collections = 0;
    let mut records = [(0u32, [0u32; 3]); 8];
    let mut record_count = 0;
    let mut fields = 0u32;
    let mut local_count = 0;
    let mut usage_min = None;
    let mut usage_max = None;
    let mut needs_usage_page = false;
    let mut offset = 0;
    let mut items = 0;
    let mut numbered = false;
    let mut unnumbered = false;
    while offset < bytes.len() {
        items += 1;
        if items > 256 {
            return Err("descriptor items");
        }
        let prefix = bytes[offset];
        offset += 1;
        if prefix == 0xfe {
            return Err("long item unsupported");
        }
        let size = [0, 1, 2, 4][usize::from(prefix & 3)];
        let payload = bytes.get(offset..offset + size).ok_or("truncated item")?;
        offset += size;
        let mut value = 0u32;
        for (index, byte) in payload.iter().enumerate() {
            value |= u32::from(*byte) << (index * 8);
        }
        let kind = (prefix >> 2) & 3;
        let tag = prefix >> 4;
        match (kind, tag) {
            (1, 10) => {
                if size != 0 || depth == stack.len() {
                    return Err("global push");
                }
                stack[depth] = state;
                depth += 1;
            }
            (1, 11) => {
                if size != 0 || depth == 0 {
                    return Err("global pop");
                }
                depth -= 1;
                state = stack[depth];
            }
            (1, 0..=9) => {
                if size == 0 {
                    return Err("empty global value");
                }
                match tag {
                    0 => {
                        if value > 65535 {
                            return Err("usage page");
                        }
                        state.has_usage_page = true;
                    }
                    7 => {
                        if value == 0 || value > 32 {
                            return Err("report width");
                        }
                        state.size = value;
                    }
                    8 => {
                        if size != 1 || value == 0 {
                            return Err("report ID");
                        }
                        state.id = value;
                    }
                    9 => {
                        if value == 0 || value > 256 {
                            return Err("report count");
                        }
                        state.count = value;
                    }
                    _ => {}
                }
            }
            (2, 0..=5 | 7..=9) => {
                if matches!(tag, 1 | 2) && size == 4 {
                    return Err("extended usage range unsupported");
                }
                local_count += 1;
                if size == 0 || local_count > 32 {
                    return Err("local items");
                }
                needs_usage_page |= tag <= 2 && size < 4;
                match tag {
                    1 => usage_min = Some(value),
                    2 => usage_max = Some(value),
                    _ => {}
                }
            }
            (0, 8..=12) => {
                if needs_usage_page && !state.has_usage_page {
                    return Err("missing usage page");
                }
                match (usage_min, usage_max) {
                    (Some(min), Some(max))
                        if max >= min && max - min < 256 && min >> 16 == max >> 16 => {}
                    (None, None) => {}
                    _ => return Err("usage range"),
                }
                match tag {
                    10 => {
                        if size != 1 || value > 6 || collections == 8 {
                            return Err("collection");
                        }
                        collections += 1;
                    }
                    12 => {
                        if size != 0 || collections == 0 {
                            return Err("end collection");
                        }
                        collections -= 1;
                    }
                    _ => {
                        if size == 0 || state.size == 0 || state.count == 0 {
                            return Err("report attributes");
                        }
                        fields = fields.checked_add(state.count).ok_or("fields")?;
                        if fields > 256 {
                            return Err("fields");
                        }
                        let index = match records[..record_count]
                            .iter()
                            .position(|(id, _)| *id == state.id)
                        {
                            Some(index) => index,
                            None => {
                                if record_count == records.len() {
                                    return Err("report IDs");
                                }
                                let index = record_count;
                                record_count += 1;
                                records[index].0 = state.id;
                                index
                            }
                        };
                        let category = match tag {
                            8 => 0,
                            9 => 1,
                            _ => 2,
                        };
                        let bits = &mut records[index].1[category];
                        *bits = bits
                            .checked_add(state.size.checked_mul(state.count).ok_or("report bits")?)
                            .ok_or("report bits")?;
                        if *bits > 512 {
                            return Err("report bits");
                        }
                        numbered |= state.id != 0;
                        unnumbered |= state.id == 0;
                        if numbered && unnumbered {
                            return Err("mixed report numbering");
                        }
                    }
                }
                local_count = 0;
                usage_min = None;
                usage_max = None;
                needs_usage_page = false;
            }
            _ => return Err("unsupported item"),
        }
    }
    if depth != 0 || collections != 0 || record_count == 0 || local_count != 0 {
        return Err("incomplete descriptor");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hidreport::Report;

    const KEY: &[u8] = &[
        0x05, 7, 0x09, 4, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 0x81, 2,
    ];

    #[test]
    fn extended_usage_ranges_are_rejected_before_page_misinterpretation() {
        let bytes = [
            0x05, 7, 0x1b, 0xe9, 0, 12, 0, 0x2b, 0xea, 0, 12, 0, 0x15, 0, 0x25, 1, 0x75, 1, 0x95,
            2, 0x81, 2,
        ];
        assert_eq!(preflight(&bytes), Err("extended usage range unsupported"));
        let explicit = [
            0x0b, 0xe9, 0, 12, 0, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 0x81, 2,
        ];
        preflight(&explicit).unwrap();
        let parsed = hidreport::ReportDescriptor::try_from(explicit.as_slice()).unwrap();
        let hidreport::Field::Variable(field) = &parsed.input_reports()[0].fields()[0] else {
            panic!()
        };
        assert_eq!(u32::from(field.usage), 0xc00e9);
    }

    #[test]
    fn accepts_variable_and_restored_globals() {
        preflight(KEY).unwrap();
        let mut bytes = KEY[..12].to_vec();
        bytes.extend_from_slice(&[0xa4, 0x95, 2, 0xb4, 0x81, 2]);
        preflight(&bytes).unwrap();
        let parsed = hidreport::ReportDescriptor::try_from(bytes.as_slice()).unwrap();
        assert_eq!(parsed.input_reports()[0].fields().len(), 1);
    }

    #[test]
    fn rejects_truncation_and_unbounded_counts_before_parser() {
        for end in 0..KEY.len() {
            assert!(preflight(&KEY[..end]).is_err(), "prefix {end}");
        }
        for bytes in [
            &[0x77, 255, 255, 255, 255, 0x95, 2, 0x81, 3][..],
            &[0x75, 1, 0x97, 255, 255, 255, 255, 0x81, 3],
            &[0xb4],
            &[0xc0],
            &[0xfe, 0, 0],
            &[0xa4],
        ] {
            assert!(preflight(bytes).is_err());
        }
    }

    #[test]
    fn accepts_keyboard_array_and_rejects_expanded_usage_range() {
        let mut bytes = vec![
            0x05, 7, 0x19, 0, 0x29, 255, 0x15, 0, 0x26, 255, 0, 0x75, 8, 0x95, 6, 0x81, 0,
        ];
        preflight(&bytes).unwrap();
        assert!(hidreport::ReportDescriptor::try_from(bytes.as_slice()).is_ok());
        bytes.splice(4..6, [0x2a, 0, 1]);
        assert_eq!(preflight(&bytes), Err("usage range"));
    }

    #[test]
    fn bounds_total_report_size_and_rejects_mixed_numbering() {
        let mut bytes = KEY.to_vec();
        bytes.extend_from_slice(&[0x85, 1, 0x09, 4, 0x81, 2]);
        assert_eq!(preflight(&bytes), Err("mixed report numbering"));
        assert!(preflight(&[0x75, 32, 0x95, 16, 0x81, 3]).is_ok());
        assert_eq!(
            preflight(&[0x75, 32, 0x95, 17, 0x81, 3]),
            Err("report bits")
        );
    }

    #[test]
    fn accepted_single_byte_mutations_do_not_panic_in_candidate() {
        let mut accepted = 0;
        for index in 0..KEY.len() {
            for byte in 0..=255 {
                let mut bytes = KEY.to_vec();
                bytes[index] = byte;
                if preflight(&bytes).is_ok() {
                    accepted += 1;
                    assert!(
                        std::panic::catch_unwind(|| hidreport::ReportDescriptor::try_from(
                            bytes.as_slice()
                        ))
                        .is_ok(),
                        "index {index}, byte {byte}"
                    );
                }
            }
        }
        assert!(accepted > 100);
        println!("Candidate parsed {accepted} preflight-accepted mutations without panic");
    }

    #[test]
    fn global_push_pop_does_not_restore_local_usages() {
        let bytes = [
            0x05, 7, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 2, 0x09, 4, 0xa4, 0x09, 5, 0xb4, 0x81, 2,
        ];
        preflight(&bytes).unwrap();
        let parsed = hidreport::ReportDescriptor::try_from(bytes.as_slice()).unwrap();
        let report = &parsed.input_reports()[0];
        let hidreport::Field::Variable(first) = &report.fields()[0] else {
            panic!()
        };
        let hidreport::Field::Variable(second) = &report.fields()[1] else {
            panic!()
        };
        assert_ne!(
            first.usage, second.usage,
            "local usages survive global Push/Pop"
        );
    }

    #[test]
    fn usage_page_is_checked_after_global_restore() {
        let bytes = [0x75, 1, 0x95, 1, 0xa4, 0x05, 7, 0x09, 4, 0xb4, 0x81, 2];
        assert_eq!(preflight(&bytes), Err("missing usage page"));
    }

    #[test]
    fn keyboard_array_extracts_each_slot_and_rejects_short_payload() {
        let bytes = [
            0x05, 7, 0x19, 0, 0x29, 255, 0x15, 0, 0x26, 255, 0, 0x75, 8, 0x95, 6, 0x81, 0,
        ];
        preflight(&bytes).unwrap();
        let parsed = hidreport::ReportDescriptor::try_from(bytes.as_slice()).unwrap();
        let report = &parsed.input_reports()[0];
        assert_eq!(report.size_in_bytes(), 6);
        let hidreport::Field::Array(field) = &report.fields()[0] else {
            panic!()
        };
        let values: Vec<u32> = field
            .extract(&[0x4e, 0x4b, 0, 0, 0, 0])
            .unwrap()
            .into_iter()
            .map(u32::from)
            .collect();
        assert_eq!(values, [0x4e, 0x4b, 0, 0, 0, 0]);
        validate_input(&parsed, &[0x4e, 0x4b, 0, 0, 0, 0]).unwrap();
        assert!(validate_input(&parsed, &[0x4e]).is_err());
        assert!(validate_input(&parsed, &[]).is_err());
        assert!(validate_input(&parsed, &[0; 7]).is_err());
        let frame = frame_ble_input(&parsed, &[0, 1], &[0x4e, 0x4b, 0, 0, 0, 0]).unwrap();
        assert_eq!(frame.as_bytes(), &[0x4e, 0x4b, 0, 0, 0, 0]);
        assert!(frame_ble_input(&parsed, &[1, 1], &[0; 6]).is_err());
    }

    #[test]
    fn numbered_consumer_fields_require_report_id_framing() {
        let bytes = [
            0x05, 0x0c, 0x09, 1, 0xa1, 1, 0x85, 2, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 2, 0x09, 0xe9,
            0x09, 0xea, 0x81, 2, 0x75, 6, 0x95, 1, 0x81, 3, 0xc0,
        ];
        preflight(&bytes).unwrap();
        let parsed = hidreport::ReportDescriptor::try_from(bytes.as_slice()).unwrap();
        let report = parsed.find_input_report(&[2, 1]).unwrap();
        assert_eq!(report.size_in_bytes(), 2);
        let hidreport::Field::Variable(first) = &report.fields()[0] else {
            panic!()
        };
        let hidreport::Field::Variable(second) = &report.fields()[1] else {
            panic!()
        };
        assert_eq!(u32::from(first.extract(&[2, 1]).unwrap()), 1);
        assert_eq!(u32::from(second.extract(&[2, 1]).unwrap()), 0);
        assert_eq!(u32::from(second.extract(&[2, 2]).unwrap()), 1);
        validate_input(&parsed, &[2, 1]).unwrap();
        assert!(validate_input(&parsed, &[2]).is_err());
        assert!(validate_input(&parsed, &[]).is_err());
        assert!(validate_input(&parsed, &[2, 1, 0]).is_err());
        assert!(parsed.find_input_report(&[3, 1]).is_none());
        let frame = frame_ble_input(&parsed, &[2, 1], &[1]).unwrap();
        assert_eq!(frame.as_bytes(), &[2, 1]);
        assert_eq!(u32::from(first.extract(frame.as_bytes()).unwrap()), 1);
        for reference in [
            &[][..],
            &[2],
            &[2, 1, 0],
            &[0, 1],
            &[3, 1],
            &[2, 2],
            &[2, 3],
        ] {
            assert!(frame_ble_input(&parsed, reference, &[1]).is_err());
        }
        for payload in [&[][..], &[2, 1], &[0; 65]] {
            assert!(frame_ble_input(&parsed, &[2, 1], payload).is_err());
        }
    }
}
