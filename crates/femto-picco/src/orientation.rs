//! Lossless transforms between logical view and calibrated portrait coordinates.
use crate::{Error, HEIGHT, PLANE_BYTES, WIDTH};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Orientation {
    #[default]
    Portrait,
    /// The physical device is turned clockwise from its portrait position.
    Clockwise,
    Inverted,
    Counterclockwise,
}
impl Orientation {
    pub const fn dimensions(self) -> (usize, usize) {
        match self {
            Self::Portrait | Self::Inverted => (HEIGHT, WIDTH),
            Self::Clockwise | Self::Counterclockwise => (WIDTH, HEIGHT),
        }
    }
    pub fn to_portrait(self, x: usize, y: usize) -> Option<(usize, usize)> {
        let (width, height) = self.dimensions();
        if x >= width || y >= height {
            return None;
        }
        Some(match self {
            Self::Portrait => (x, y),
            Self::Clockwise => (y, WIDTH - 1 - x),
            Self::Inverted => (HEIGHT - 1 - x, WIDTH - 1 - y),
            Self::Counterclockwise => (HEIGHT - 1 - y, x),
        })
    }
    pub fn from_portrait(self, x: usize, y: usize) -> Option<(usize, usize)> {
        if x >= HEIGHT || y >= WIDTH {
            return None;
        }
        Some(match self {
            Self::Portrait => (x, y),
            Self::Clockwise => (WIDTH - 1 - y, x),
            Self::Inverted => (HEIGHT - 1 - x, WIDTH - 1 - y),
            Self::Counterclockwise => (y, HEIGHT - 1 - x),
        })
    }
    /// Calls `write(native_bit_index, pixel)` for every view pixel. Pixels are
    /// read sequentially because the frame is far larger than the data cache;
    /// the native index is affine in the view coordinates.
    fn scan(self, pixels: &[u8], mut write: impl FnMut(usize, u8)) {
        let native = |x: usize, y: usize| {
            let (px, py) = self.to_portrait(x, y).expect("bounded view coordinates");
            ((HEIGHT - 1 - px) * WIDTH + py) as isize
        };
        let (width, _) = self.dimensions();
        let (column_step, row_step) = (native(1, 0) - native(0, 0), native(0, 1) - native(0, 0));
        for (y, row) in pixels.chunks_exact(width).enumerate() {
            let mut index = native(0, 0) + y as isize * row_step;
            for &pixel in row {
                write(index as usize, pixel);
                index += column_step;
            }
        }
    }

    /// Packs already quantized pixels without resampling or an intermediate frame.
    /// Invalid sizes or encodings leave the output unchanged.
    pub fn monochrome(
        self,
        pixels: &[u8],
        plane: &mut [u8],
    ) -> Result<(), Error<core::convert::Infallible>> {
        if pixels.len() != WIDTH * HEIGHT || plane.len() != PLANE_BYTES {
            return Err(Error::FrameSize);
        }
        if pixels.iter().any(|p| !matches!(p, 0 | 255)) {
            return Err(Error::FrameEncoding);
        }
        plane.fill(0);
        self.scan(pixels, |native, pixel| {
            plane[native / 8] |= (pixel & 0x80) >> (native % 8);
        });
        Ok(())
    }
    /// Packs four-level pixels into the panel's absolute high and low planes.
    pub fn grayscale(
        self,
        pixels: &[u8],
        msb: &mut [u8],
        lsb: &mut [u8],
    ) -> Result<(), Error<core::convert::Infallible>> {
        if pixels.len() != WIDTH * HEIGHT || msb.len() != PLANE_BYTES || lsb.len() != PLANE_BYTES {
            return Err(Error::FrameSize);
        }
        if pixels.iter().any(|p| !matches!(p, 0 | 85 | 170 | 255)) {
            return Err(Error::FrameEncoding);
        }
        msb.fill(0);
        lsb.fill(0);
        // 0, 85, 170 and 255 carry their two-bit level in bits 7 and 6.
        self.scan(pixels, |native, pixel| {
            msb[native / 8] |= (pixel & 0x80) >> (native % 8);
            lsb[native / 8] |= ((pixel << 1) & 0x80) >> (native % 8);
        });
        Ok(())
    }
}
