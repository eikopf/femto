//! Validated input-report routes for one discovered HID service.

use crate::RemoteInput;

pub const MAX_DESCRIPTORS: usize = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DescriptorKind {
    ReportReference,
    ClientConfiguration,
    #[default]
    Other,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Descriptor {
    pub handle: u16,
    pub kind: DescriptorKind,
}

pub struct ReportDescriptors {
    reference: u16,
    configuration: u16,
}

impl ReportDescriptors {
    /// The backend supplies descriptors owned by this characteristic, in handle order.
    pub fn new(
        value_handle: u16,
        last_handle: u16,
        descriptors: &[Descriptor],
    ) -> Result<Self, &'static str> {
        if value_handle == 0
            || last_handle <= value_handle
            || descriptors.is_empty()
            || descriptors.len() > MAX_DESCRIPTORS
        {
            return Err("GATT descriptor bounds");
        }
        let mut reference = None;
        let mut configuration = None;
        let mut previous = value_handle;
        for descriptor in descriptors {
            if descriptor.handle <= previous || descriptor.handle > last_handle {
                return Err("GATT descriptor handle order or range");
            }
            previous = descriptor.handle;
            let slot = match descriptor.kind {
                DescriptorKind::ReportReference => &mut reference,
                DescriptorKind::ClientConfiguration => &mut configuration,
                DescriptorKind::Other => continue,
            };
            if slot.replace(descriptor.handle).is_some() {
                return Err("duplicate GATT report descriptor");
            }
        }
        Ok(Self {
            reference: reference.ok_or("missing Report Reference")?,
            configuration: configuration.ok_or("missing Client Configuration")?,
        })
    }

    pub fn reference_handle(&self) -> u16 {
        self.reference
    }
    pub fn configuration_handle(&self) -> u16 {
        self.configuration
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputReport {
    pub value_handle: u16,
    pub cccd_handle: u16,
    pub reference: [u8; 2],
}

pub struct ReportRoutes {
    reports: [InputReport; 8],
    len: usize,
}

impl ReportRoutes {
    /// Requires exactly one notifying characteristic for every input report ID.
    /// The backend supplies only input reports from the same HID service/map.
    pub fn new(input: &RemoteInput, reports: &[InputReport]) -> Result<Self, &'static str> {
        if reports.is_empty() || reports.len() > 8 || reports.len() != input.report_ids().count() {
            return Err("GATT input report count");
        }
        for (index, report) in reports.iter().enumerate() {
            if report.value_handle == 0 || report.cccd_handle <= report.value_handle {
                return Err("GATT report handles");
            }
            if report.reference[1] != 1 || !input.report_ids().any(|id| id == report.reference[0]) {
                return Err("GATT report reference");
            }
            for previous in &reports[..index] {
                if previous.reference == report.reference
                    || previous.value_handle == report.value_handle
                    || previous.cccd_handle == report.cccd_handle
                    || previous.value_handle == report.cccd_handle
                    || previous.cccd_handle == report.value_handle
                {
                    return Err("duplicate GATT report route");
                }
            }
        }
        let mut result = Self {
            reports: [InputReport::default(); 8],
            len: reports.len(),
        };
        result.reports[..reports.len()].copy_from_slice(reports);
        Ok(result)
    }

    pub fn reports(&self) -> &[InputReport] {
        &self.reports[..self.len]
    }

    pub fn reference(&self, value_handle: u16) -> Option<[u8; 2]> {
        self.reports()
            .iter()
            .find(|report| report.value_handle == value_handle)
            .map(|report| report.reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_must_be_unique_and_belong_to_the_characteristic_range() {
        let reference = Descriptor {
            handle: 11,
            kind: DescriptorKind::ReportReference,
        };
        let configuration = Descriptor {
            handle: 12,
            kind: DescriptorKind::ClientConfiguration,
        };
        let valid = ReportDescriptors::new(10, 20, &[reference, configuration]).unwrap();
        assert_eq!(valid.reference_handle(), 11);
        assert_eq!(valid.configuration_handle(), 12);
        for entries in [
            vec![],
            vec![reference],
            vec![configuration],
            vec![configuration, reference],
            vec![
                reference,
                Descriptor {
                    handle: 11,
                    ..configuration
                },
            ],
            vec![
                reference,
                Descriptor {
                    handle: 21,
                    ..configuration
                },
            ],
            vec![
                reference,
                configuration,
                Descriptor {
                    handle: 13,
                    ..reference
                },
            ],
        ] {
            assert!(ReportDescriptors::new(10, 20, &entries).is_err());
        }
        let mut entries = vec![reference, configuration];
        entries.extend((13..27).map(|handle| Descriptor {
            handle,
            ..Default::default()
        }));
        assert!(ReportDescriptors::new(10, 30, &entries).is_ok());
        entries.push(Descriptor {
            handle: 27,
            ..Default::default()
        });
        assert!(ReportDescriptors::new(10, 30, &entries).is_err());
        assert!(ReportDescriptors::new(11, 20, &[reference, configuration]).is_err());
        assert!(ReportDescriptors::new(10, 10, &[reference, configuration]).is_err());
    }

    const ARRAY: &[u8] = &[
        0x05, 7, 0x19, 0, 0x29, 255, 0x15, 0, 0x26, 255, 0, 0x75, 8, 0x95, 6, 0x81, 0,
    ];

    fn report(id: u8, handle: u16) -> InputReport {
        InputReport {
            value_handle: handle,
            cccd_handle: handle + 1,
            reference: [id, 1],
        }
    }

    #[test]
    fn routes_numbered_notifications_without_adding_payload_ids() {
        let mut descriptor = vec![0x85, 3];
        descriptor.extend_from_slice(ARRAY);
        descriptor.extend_from_slice(&[0x85, 9]);
        descriptor.extend_from_slice(ARRAY);
        let mut input = RemoteInput::new(&descriptor).unwrap();
        let routes = ReportRoutes::new(&input, &[report(9, 30), report(3, 10)]).unwrap();
        assert_eq!(
            input
                .accept(&routes.reference(10).unwrap(), &[0x4e, 0, 0, 0, 0, 0])
                .unwrap()
                .as_slice(),
            &[0x7004e]
        );
        assert!(
            input
                .accept(&routes.reference(30).unwrap(), &[0x4e, 0, 0, 0, 0, 0])
                .unwrap()
                .as_slice()
                .is_empty()
        );
        assert_eq!(routes.reference(11), None);
        assert_eq!(routes.reference(0), None);
        for invalid in [
            vec![],
            vec![report(3, 10)],
            vec![report(3, 10), report(3, 30)],
            vec![report(3, 10), report(8, 30)],
            vec![report(3, 10), report(9, 10)],
            vec![report(3, 10), report(9, 11)],
        ] {
            assert!(ReportRoutes::new(&input, &invalid).is_err());
        }
    }

    #[test]
    fn unnumbered_reference_and_handle_validation() {
        let input = RemoteInput::new(ARRAY).unwrap();
        assert!(ReportRoutes::new(&input, &[report(0, 10)]).is_ok());
        for invalid in [
            report(1, 10),
            report(0, 0),
            InputReport {
                reference: [0, 2],
                ..report(0, 10)
            },
            InputReport {
                cccd_handle: 10,
                ..report(0, 10)
            },
            InputReport {
                cccd_handle: 9,
                ..report(0, 10)
            },
        ] {
            assert!(ReportRoutes::new(&input, &[invalid]).is_err());
        }
        assert!(ReportRoutes::new(&input, &[report(0, 10); 9]).is_err());
    }
}
