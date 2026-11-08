//! Bounded legacy BLE advertisement inventory.

pub const CAPACITY: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Advertisement {
    pub address: [u8; 6],
    pub address_type: u8,
    pub rssi: i8,
    advertising: [u8; 31],
    advertising_len: u8,
    response: [u8; 31],
    response_len: u8,
}

fn fields(mut bytes: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    std::iter::from_fn(move || {
        let len = usize::from(*bytes.first()?);
        if len == 0 || len >= bytes.len() {
            bytes = &[];
            return None;
        }
        let field = (bytes[1], &bytes[2..=len]);
        bytes = &bytes[len + 1..];
        Some(field)
    })
}

fn valid(mut bytes: &[u8]) -> bool {
    if bytes.len() > 31 {
        return false;
    }
    while let Some(&len) = bytes.first() {
        if len == 0 {
            return bytes.iter().all(|byte| *byte == 0);
        }
        let Some(rest) = bytes.get(usize::from(len) + 1..) else {
            return false;
        };
        bytes = rest;
    }
    true
}

impl Advertisement {
    pub fn name(&self) -> Option<&str> {
        let fields = || {
            fields(&self.advertising[..usize::from(self.advertising_len)])
                .chain(fields(&self.response[..usize::from(self.response_len)]))
        };
        for kind in [9, 8] {
            for (tag, value) in fields() {
                if tag == kind
                    && let Ok(name) = std::str::from_utf8(value)
                    && !name.is_empty()
                    && !name.chars().any(char::is_control)
                {
                    return Some(name);
                }
            }
        }
        None
    }

    pub fn advertises_hid(&self) -> bool {
        fields(&self.advertising[..usize::from(self.advertising_len)])
            .chain(fields(&self.response[..usize::from(self.response_len)]))
            .any(|(tag, value)| {
                matches!(tag, 2 | 3)
                    && value.len() % 2 == 0
                    && value.as_chunks::<2>().0.contains(&[0x12, 0x18])
            })
    }
}

#[derive(Default)]
pub struct Inventory {
    entries: [Option<Advertisement>; CAPACITY],
    len: usize,
    pub dropped: u32,
}

impl Inventory {
    pub fn entries(&self) -> impl Iterator<Item = &Advertisement> {
        self.entries[..self.len].iter().flatten()
    }

    /// Empty data retains the previous packet for an existing address and type.
    pub fn record(
        &mut self,
        address: [u8; 6],
        address_type: u8,
        rssi: i8,
        advertising: &[u8],
        response: &[u8],
    ) -> Result<(), &'static str> {
        if address_type > 3 || !valid(advertising) || !valid(response) {
            return Err("invalid legacy BLE advertisement");
        }
        let existing = self
            .entries()
            .position(|entry| entry.address == address && entry.address_type == address_type);
        let index = match existing {
            Some(index) => index,
            None if self.len < CAPACITY => {
                let index = self.len;
                self.len += 1;
                self.entries[index] = Some(Advertisement {
                    address,
                    address_type,
                    rssi,
                    advertising: [0; 31],
                    advertising_len: 0,
                    response: [0; 31],
                    response_len: 0,
                });
                index
            }
            None => {
                self.dropped = self.dropped.saturating_add(1);
                return Ok(());
            }
        };
        let entry = self.entries[index].as_mut().ok_or("missing scan entry")?;
        entry.rssi = rssi;
        if !advertising.is_empty() {
            entry.advertising[..advertising.len()].copy_from_slice(advertising);
            entry.advertising_len = advertising.len() as u8;
        }
        if !response.is_empty() {
            entry.response[..response.len()].copy_from_slice(response);
            entry.response_len = response.len() as u8;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_responses_and_keeps_address_types_separate() {
        let mut inventory = Inventory::default();
        inventory
            .record([1; 6], 0, -80, &[3, 3, 0x12, 0x18], &[2, 8, b'A'])
            .unwrap();
        inventory
            .record([1; 6], 0, -40, &[], &[3, 9, b'B', b'C'])
            .unwrap();
        let entry = inventory.entries().next().unwrap();
        assert_eq!(entry.name(), Some("BC"));
        assert!(entry.advertises_hid());
        assert_eq!(entry.rssi, -40);
        inventory.record([1; 6], 1, -50, &[], &[]).unwrap();
        assert_eq!(inventory.entries().count(), 2);
    }

    #[test]
    fn malformed_data_does_not_replace_existing_entry() {
        let mut inventory = Inventory::default();
        inventory
            .record([1; 6], 0, -80, &[2, 9, b'A'], &[])
            .unwrap();
        let before = *inventory.entries().next().unwrap();
        for bytes in [&[31, 9][..], &[0, 9], &[1; 32]] {
            assert!(inventory.record([1; 6], 0, -20, bytes, &[]).is_err());
            assert_eq!(*inventory.entries().next().unwrap(), before);
        }
        assert!(inventory.record([1; 6], 4, -20, &[], &[]).is_err());
        inventory
            .record([2; 6], 0, -30, &[2, 9, 0xff], &[])
            .unwrap();
        inventory
            .record([3; 6], 0, -30, &[2, 9, b'\n'], &[])
            .unwrap();
        assert!(
            inventory
                .entries()
                .skip(1)
                .all(|entry| entry.name().is_none())
        );
    }

    #[test]
    fn full_inventory_updates_known_peers_without_eviction() {
        let mut inventory = Inventory::default();
        for byte in 0..16 {
            inventory.record([byte; 6], 0, -80, &[], &[]).unwrap();
        }
        inventory.record([17; 6], 0, -10, &[], &[]).unwrap();
        inventory.record([0; 6], 0, -10, &[], &[]).unwrap();
        assert_eq!(inventory.entries().count(), CAPACITY);
        assert_eq!(inventory.dropped, 1);
        assert_eq!(inventory.entries().next().unwrap().rssi, -10);
    }
}
