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
    fn native_index(self, x: usize, y: usize) -> usize {
        let (px, py) = self.to_portrait(x, y).expect("bounded view coordinates");
        (HEIGHT - 1 - px) * WIDTH + py
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
        let (width, height) = self.dimensions();
        plane.fill(255);
        for y in 0..height {
            for x in 0..width {
                if pixels[y * width + x] == 0 {
                    let native = self.native_index(x, y);
                    plane[native / 8] &= !(0x80 >> (native % 8));
                }
            }
        }
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
        let (width, height) = self.dimensions();
        msb.fill(0);
        lsb.fill(0);
        for y in 0..height {
            for x in 0..width {
                let value = pixels[y * width + x] / 85;
                let native = self.native_index(x, y);
                let mask = 0x80 >> (native % 8);
                if value & 2 != 0 {
                    msb[native / 8] |= mask;
                }
                if value & 1 != 0 {
                    lsb[native / 8] |= mask;
                }
            }
        }
        Ok(())
    }
}
