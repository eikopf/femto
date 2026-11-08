//! Raster controls independent of shaping and page layout.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextRaster {
    pub hinting: bool,
    /// Extra outline weight in physical pixels; does not change advances.
    #[serde(deserialize_with = "crate::deserialize_f32")]
    pub stroke: f32,
    /// Exponent applied to ink coverage. Values below one darken edge pixels.
    #[serde(deserialize_with = "crate::deserialize_f32")]
    pub coverage_gamma: f32,
    /// Preserve fractional horizontal positions using grayscale coverage.
    pub fractional: bool,
}

impl Default for TextRaster {
    fn default() -> Self {
        Self {
            hinting: true,
            stroke: 0.0,
            coverage_gamma: 1.0,
            fractional: false,
        }
    }
}

impl TextRaster {
    pub fn validate(self) -> Result<()> {
        if !self.stroke.is_finite() || !(0.0..=0.75).contains(&self.stroke) {
            return Err(Error::Layout(
                "text stroke must be between 0 and 0.75 pixels",
            ));
        }
        if !self.coverage_gamma.is_finite() || !(0.3..=3.0).contains(&self.coverage_gamma) {
            return Err(Error::Layout(
                "text coverage gamma must be between 0.3 and 3",
            ));
        }
        Ok(())
    }

    pub fn coverage_table(self) -> Result<[u8; 256]> {
        self.validate()?;
        Ok(std::array::from_fn(|i| {
            ((i as f32 / 255.0).powf(self.coverage_gamma) * 255.0).round() as u8
        }))
    }
}

/// Quantizes text without introducing spatial patterns into glyph strokes.
pub(crate) fn text_level(value: u8, levels: u16) -> u8 {
    let steps = levels - 1;
    (((value as u16 * steps + 127) / 255) * 255 / steps) as u8
}
