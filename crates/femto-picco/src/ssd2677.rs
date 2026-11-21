use crate::{Bus, Display, Error, Refresh, waveform};

// FreeInk Picco SE0400NQW47 initialization; see THIRD-PARTY.md.
const INIT: &[(u8, &[u8])] = &[
    (0x00, &[0x27, 0x09]),
    (
        0x83,
        &[0x00, 0x00, 0x03, 0x1f, 0x00, 0xc8, 0x02, 0xa8, 0x01],
    ),
    (0x01, &[0x07, 0xf0, 0x22, 0x20, 0x78, 0x22]),
    (0x06, &[0x0f, 0x8b, 0x93, 0xc1]),
    (0xe7, &[0xc1]),
    (0x50, &[0x77]),
    (0x62, &[0x98, 0x98, 0x98, 0x75, 0xca, 0xb2, 0x98, 0x7e]),
    (0xe0, &[0x10]),
    (0x65, &[0, 0, 0, 0]),
    (0x30, &[0x04]),
    (0x82, &[0x28]),
    (0xe9, &[0x01]),
];

/// Spreads the bits of a byte to the even positions of a word.
const SPREAD: [u16; 256] = {
    let mut table = [0; 256];
    let mut value = 0;
    while value < 256 {
        let mut bit = 0;
        while bit < 8 {
            table[value] |= ((value as u16 >> bit) & 1) << (2 * bit);
            bit += 1;
        }
        value += 1;
    }
    table
};

/// Interleaves two bitplanes into the controller's two-bit pixels.
fn pack(old: u8, new: u8) -> [u8; 2] {
    ((SPREAD[old as usize] << 1) | SPREAD[new as usize]).to_be_bytes()
}

impl<B: Bus> Display<B> {
    pub(super) fn grayscale_ssd2677(
        &mut self,
        msb: &[u8],
        lsb: &[u8],
    ) -> Result<(), Error<B::Error>> {
        self.idle()?;
        self.command(0x04, &[])?;
        self.bus.delay_ms(10);
        self.idle()?;
        let packed = msb
            .iter()
            .zip(lsb)
            .flat_map(|(&high, &low)| pack(high, low));
        self.bus.command_stream(0x10, packed).map_err(Error::Bus)?;
        self.command(0x20, &waveform::SE0400_GRAY)?;
        self.command(0x12, &[0])?;
        self.bus.delay_ms(10);
        self.idle()?;
        self.command(0x02, &[0])?;
        self.bus.delay_ms(10);
        self.idle()?;
        self.previous.copy_from_slice(msb);
        self.ready = true;
        Ok(())
    }

    /// Shows the black-and-white base of a four-level frame: only white stays
    /// white and both grays are drawn as ink. `previous` becomes the base, so
    /// later differential refreshes stay valid with or without the overlay.
    pub(super) fn antialiased_base_ssd2677(
        &mut self,
        msb: &[u8],
        lsb: &[u8],
        mode: Refresh,
    ) -> Result<(), Error<B::Error>> {
        let mut base = core::mem::take(&mut self.scratch);
        base.clear();
        base.extend(msb.iter().zip(lsb).map(|(&high, &low)| high & low));
        if mode == Refresh::Fast {
            // Ink that an earlier overlay lightened still needs the full
            // white-to-black pulse, so treat every incoming ink pixel as white.
            for (old, &new) in self.previous.iter_mut().zip(&base) {
                *old |= !new;
            }
        }
        let shown = self.refresh_ssd2677(&base, mode);
        self.scratch = base;
        shown
    }

    /// Moves the gray pixels of the frame whose base is on the panel,
    /// without disturbing the rest.
    pub(super) fn overlay_ssd2677(
        &mut self,
        msb: &[u8],
        lsb: &[u8],
    ) -> Result<(), Error<B::Error>> {
        self.idle()?;
        // High bit: either gray. Low bit: dark gray.
        let masks = msb
            .iter()
            .zip(lsb)
            .flat_map(|(&high, &low)| pack(high ^ low, !high & low));
        self.bus.command_stream(0x10, masks).map_err(Error::Bus)?;
        self.command(0x20, &waveform::SE0400_OVERLAY)?;
        self.command(0x04, &[])?;
        self.bus.delay_ms(10);
        self.idle()?;
        self.command(0x12, &[0])?;
        self.bus.delay_ms(10);
        self.idle()?;
        self.command(0x02, &[0])?;
        self.bus.delay_ms(10);
        self.ready = true;
        Ok(())
    }

    pub(super) fn initialize_ssd2677(&mut self) -> Result<(), Error<B::Error>> {
        self.idle()?;
        for &(command, data) in INIT {
            self.command(command, data)?;
            if command == 0 {
                self.bus.delay_ms(10);
            }
        }
        self.bus.delay_ms(10);
        self.idle()?;
        self.previous.fill(255);
        self.ready = true;
        Ok(())
    }

    pub(super) fn refresh_ssd2677(
        &mut self,
        plane: &[u8],
        mode: Refresh,
    ) -> Result<(), Error<B::Error>> {
        self.idle()?;
        let packed = self
            .previous
            .iter()
            .zip(plane)
            .flat_map(|(&old, &new)| pack(old, new));
        self.bus.command_stream(0x10, packed).map_err(Error::Bus)?;
        self.command(
            0x20,
            if mode == Refresh::Fast {
                &waveform::SE0400_FAST
            } else {
                &waveform::SE0400_FULL
            },
        )?;
        self.command(0x04, &[])?;
        self.bus.delay_ms(10);
        self.idle()?;
        self.command(0x12, &[0])?;
        self.bus.delay_ms(10);
        self.idle()?;
        // Power-off completes in the background; every sequence starts idle.
        self.command(0x02, &[0])?;
        self.bus.delay_ms(10);
        self.previous.copy_from_slice(plane);
        self.ready = true;
        Ok(())
    }
}
