use crate::input::Presses;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageTurn {
    Previous,
    Next,
}

pub struct Bindings {
    entries: [(u32, PageTurn); 16],
    len: usize,
}

impl Default for Bindings {
    fn default() -> Self {
        let mut entries = [(0, PageTurn::Next); 16];
        entries[..4].copy_from_slice(&[
            (0x7004b, PageTurn::Previous),
            (0x70050, PageTurn::Previous),
            (0x7004e, PageTurn::Next),
            (0x7004f, PageTurn::Next),
        ]);
        Self { entries, len: 4 }
    }
}

impl Bindings {
    pub fn new(entries: &[(u32, PageTurn)]) -> Result<Self, &'static str> {
        if entries.len() > 16 {
            return Err("binding limit");
        }
        for (index, (usage, _)) in entries.iter().enumerate() {
            let page = usage >> 16;
            let id = usage & 0xffff;
            if !matches!(page, 7 | 12) || id == 0 || (page == 7 && id <= 3) {
                return Err("unsupported binding usage");
            }
            if entries[..index]
                .iter()
                .any(|(previous, _)| previous == usage)
            {
                return Err("duplicate binding usage");
            }
        }
        let mut result = Self {
            entries: [(0, PageTurn::Next); 16],
            len: entries.len(),
        };
        result.entries[..entries.len()].copy_from_slice(entries);
        Ok(result)
    }

    /// Coalesces same-direction presses and suppresses opposing presses.
    pub fn resolve(&self, presses: &Presses) -> Option<PageTurn> {
        let mut directions = 0;
        for usage in presses.as_slice() {
            if let Some((_, direction)) = self.entries[..self.len]
                .iter()
                .find(|(bound, _)| bound == usage)
            {
                directions |= match direction {
                    PageTurn::Previous => 1,
                    PageTurn::Next => 2,
                };
            }
        }
        match directions {
            1 => Some(PageTurn::Previous),
            2 => Some(PageTurn::Next),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::RemoteInput;
    const ARRAY: &[u8] = &[
        0x05, 7, 0x19, 0, 0x29, 255, 0x15, 0, 0x26, 255, 0, 0x75, 8, 0x95, 6, 0x81, 0,
    ];

    #[test]
    fn default_actions_coalesce_and_ignore_unbound_keys() {
        let bindings = Bindings::default();
        for (payload, expected) in [
            ([0x4e, 0x4f, 0, 0, 0, 0], Some(PageTurn::Next)),
            ([0x4b, 0x50, 0, 0, 0, 0], Some(PageTurn::Previous)),
            ([0x4e, 0x4b, 0, 0, 0, 0], None),
            ([4, 0, 0, 0, 0, 0], None),
        ] {
            let mut input = RemoteInput::new(ARRAY).unwrap();
            assert_eq!(
                bindings.resolve(&input.accept(&[0, 1], &payload).unwrap()),
                expected
            );
            assert_eq!(
                bindings.resolve(&input.accept(&[0, 1], &payload).unwrap()),
                None
            );
        }
    }

    #[test]
    fn empty_configuration_disables_turns_and_custom_bindings_reverse_them() {
        let mut input = RemoteInput::new(ARRAY).unwrap();
        let pressed = input.accept(&[0, 1], &[0x4e, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(Bindings::new(&[]).unwrap().resolve(&pressed), None);
        assert_eq!(
            Bindings::new(&[(0x7004e, PageTurn::Previous)])
                .unwrap()
                .resolve(&pressed),
            Some(PageTurn::Previous)
        );
    }

    #[test]
    fn binding_validation_and_capacity() {
        let entries: Vec<_> = (4..20).map(|id| (0x70000 | id, PageTurn::Next)).collect();
        assert!(Bindings::new(&entries).is_ok());
        let mut overflow = entries.clone();
        overflow.push((0x70020, PageTurn::Next));
        assert!(Bindings::new(&overflow).is_err());
        for bad in [0x70000, 0x70001, 0x70003, 0xc0000, 0xff004e] {
            assert!(Bindings::new(&[(bad, PageTurn::Next)]).is_err());
        }
        assert!(
            Bindings::new(&[(0x7004e, PageTurn::Next), (0x7004e, PageTurn::Previous)]).is_err()
        );
    }

    #[test]
    fn consumer_volume_bindings_are_explicit() {
        let descriptor = [
            0x05, 12, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 2, 0x09, 0xe9, 0x09, 0xea, 0x81, 2, 0x75, 6,
            0x95, 1, 0x81, 3,
        ];
        let mut input = RemoteInput::new(&descriptor).unwrap();
        let presses = input.accept(&[0, 1], &[1]).unwrap();
        assert_eq!(Bindings::default().resolve(&presses), None);
        let bindings =
            Bindings::new(&[(0xc00e9, PageTurn::Previous), (0xc00ea, PageTurn::Next)]).unwrap();
        assert_eq!(bindings.resolve(&presses), Some(PageTurn::Previous));
        assert_eq!(
            bindings.resolve(&input.accept(&[0, 1], &[3]).unwrap()),
            Some(PageTurn::Next)
        );
    }
}
