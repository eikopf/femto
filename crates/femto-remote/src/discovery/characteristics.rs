use super::{MAX_CHARACTERISTICS, ServiceRange};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CharacteristicKind {
    ReportMap,
    Report,
    #[default]
    Other,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Characteristic {
    pub handle: u16,
    pub kind: CharacteristicKind,
    pub readable: bool,
    pub notifying: bool,
}

pub struct HidCharacteristics {
    map: u16,
    reports: [u16; 8],
    len: usize,
}

impl HidCharacteristics {
    /// The backend supplies the complete characteristic list sorted by value handle.
    pub fn new(
        service: ServiceRange,
        characteristics: &[Characteristic],
    ) -> Result<Self, &'static str> {
        if service.start == 0
            || service.start >= service.end
            || characteristics.is_empty()
            || characteristics.len() > MAX_CHARACTERISTICS
        {
            return Err("HID service or characteristic count");
        }
        let mut map = None;
        let mut result = Self {
            map: 0,
            reports: [0; 8],
            len: 0,
        };
        let mut previous = service.start;
        for characteristic in characteristics {
            if characteristic.handle <= previous || characteristic.handle > service.end {
                return Err("HID characteristic handle order or range");
            }
            previous = characteristic.handle;
            match characteristic.kind {
                CharacteristicKind::ReportMap => {
                    if !characteristic.readable || map.replace(characteristic.handle).is_some() {
                        return Err("duplicate or unreadable HID Report Map");
                    }
                }
                CharacteristicKind::Report if characteristic.notifying => {
                    if result.len == result.reports.len() {
                        return Err("HID notifying report limit");
                    }
                    result.reports[result.len] = characteristic.handle;
                    result.len += 1;
                }
                _ => {}
            }
        }
        result.map = map.ok_or("missing HID Report Map")?;
        if result.len == 0 {
            return Err("missing notifying HID Report");
        }
        Ok(result)
    }

    pub fn map_handle(&self) -> u16 {
        self.map
    }
    pub fn report_handles(&self) -> &[u16] {
        &self.reports[..self.len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SERVICE: ServiceRange = ServiceRange {
        start: 10,
        end: 100,
    };
    fn map() -> Characteristic {
        Characteristic {
            handle: 11,
            kind: CharacteristicKind::ReportMap,
            readable: true,
            notifying: false,
        }
    }
    fn report(handle: u16) -> Characteristic {
        Characteristic {
            handle,
            kind: CharacteristicKind::Report,
            readable: true,
            notifying: true,
        }
    }

    #[test]
    fn retains_map_and_input_candidates_at_the_limits() {
        let mut entries = vec![map()];
        entries.extend((12..20).map(report));
        entries.extend((20..43).map(|handle| Characteristic {
            handle,
            ..Default::default()
        }));
        assert_eq!(entries.len(), MAX_CHARACTERISTICS);
        let result = HidCharacteristics::new(SERVICE, &entries).unwrap();
        assert_eq!(result.map_handle(), 11);
        assert_eq!(result.report_handles(), &[12, 13, 14, 15, 16, 17, 18, 19]);
        entries.push(Characteristic {
            handle: 43,
            ..Default::default()
        });
        assert!(HidCharacteristics::new(SERVICE, &entries).is_err());
        let mut entries = vec![map()];
        entries.extend((12..21).map(report));
        assert!(HidCharacteristics::new(SERVICE, &entries).is_err());
    }

    #[test]
    fn rejects_ambiguous_missing_or_invalid_characteristics() {
        for entries in [
            vec![],
            vec![map()],
            vec![report(12)],
            vec![map(), report(11)],
            vec![map(), report(101)],
            vec![report(12), map()],
            vec![
                map(),
                Characteristic {
                    handle: 12,
                    ..map()
                },
                report(13),
            ],
            vec![
                Characteristic {
                    readable: false,
                    ..map()
                },
                report(12),
            ],
            vec![
                map(),
                Characteristic {
                    notifying: false,
                    ..report(12)
                },
            ],
        ] {
            assert!(HidCharacteristics::new(SERVICE, &entries).is_err());
        }
        for service in [
            ServiceRange { start: 0, end: 100 },
            ServiceRange { start: 12, end: 11 },
            ServiceRange {
                start: 11,
                end: 100,
            },
        ] {
            assert!(HidCharacteristics::new(service, &[map(), report(12)]).is_err());
        }
        let result = HidCharacteristics::new(
            SERVICE,
            &[
                map(),
                Characteristic {
                    notifying: false,
                    ..report(12)
                },
                report(13),
            ],
        )
        .unwrap();
        assert_eq!(result.report_handles(), &[13]);
    }
}
