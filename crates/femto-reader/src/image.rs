//! Bounded raster decoding and grayscale conversion for e-paper pages.

use std::io::Cursor;

use zune_jpeg::{
    JpegDecoder,
    zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions},
};

use crate::{Error, Result};

pub const MAX_ENCODED_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_IMAGE_PIXELS: usize = 1024 * 1024;
pub const MAX_DIMENSION: u32 = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    fn validate(self) -> Result<Self> {
        if self.width == 0
            || self.height == 0
            || self.width > MAX_DIMENSION
            || self.height > MAX_DIMENSION
            || self.width as u64 * self.height as u64 > MAX_IMAGE_PIXELS as u64
        {
            return Err(Error::Limit("image dimensions"));
        }
        Ok(self)
    }

    pub fn fit(self, max_width: u32, max_height: u32) -> Result<Self> {
        self.validate()?;
        if max_width == 0 || max_height == 0 {
            return Err(Error::Layout("empty image box"));
        }
        let scale = (max_width as f64 / self.width as f64)
            .min(max_height as f64 / self.height as f64)
            .min(1.0);
        Ok(Self {
            width: (self.width as f64 * scale).floor().max(1.0) as u32,
            height: (self.height as f64 * scale).floor().max(1.0) as u32,
        })
    }
}

pub struct GrayImage {
    pub size: Size,
    pub pixels: Vec<u8>,
}

fn encoded(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_ENCODED_BYTES {
        return Err(Error::Limit("encoded image"));
    }
    Ok(())
}

fn png_reader(bytes: &[u8]) -> Result<png::Reader<Cursor<&[u8]>>> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_limits(png::Limits {
        bytes: 2 * 1024 * 1024,
    });
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let reader = decoder
        .read_info()
        .map_err(|e| Error::Image(e.to_string()))?;
    if reader.info().animation_control.is_some() {
        return Err(Error::Image("animated PNG is unsupported".into()));
    }
    Size {
        width: reader.info().width,
        height: reader.info().height,
    }
    .validate()?;
    Ok(reader)
}

fn jpeg_reader(bytes: &[u8]) -> Result<JpegDecoder<ZCursor<&[u8]>>> {
    let options = DecoderOptions::default()
        .set_max_width(MAX_DIMENSION as usize)
        .set_max_height(MAX_DIMENSION as usize)
        .jpeg_set_out_colorspace(ColorSpace::Luma);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder
        .decode_headers()
        .map_err(|e| Error::Image(e.to_string()))?;
    let info = decoder
        .info()
        .ok_or_else(|| Error::Image("missing JPEG dimensions".into()))?;
    Size {
        width: info.width as u32,
        height: info.height as u32,
    }
    .validate()?;
    Ok(decoder)
}

pub fn dimensions(bytes: &[u8]) -> Result<Size> {
    encoded(bytes)?;
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let reader = png_reader(bytes)?;
        Ok(Size {
            width: reader.info().width,
            height: reader.info().height,
        })
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        let decoder = jpeg_reader(bytes)?;
        let info = decoder
            .info()
            .ok_or_else(|| Error::Image("missing JPEG dimensions".into()))?;
        Ok(Size {
            width: info.width as u32,
            height: info.height as u32,
        })
    } else {
        Err(Error::Image("supported image formats: PNG and JPEG".into()))
    }
}

pub fn decode(bytes: &[u8]) -> Result<GrayImage> {
    encoded(bytes)?;
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let mut reader = png_reader(bytes)?;
        let size = Size {
            width: reader.info().width,
            height: reader.info().height,
        };
        let (color, depth) = reader.output_color_type();
        if depth != png::BitDepth::Eight {
            return Err(Error::Image(
                "PNG transformation did not produce eight-bit samples".into(),
            ));
        }
        let mut pixels = Vec::with_capacity(size.width as usize * size.height as usize);
        if reader.info().interlaced {
            let length = reader
                .output_buffer_size()
                .ok_or(Error::Limit("PNG frame"))?;
            if length > MAX_IMAGE_PIXELS * 4 {
                return Err(Error::Limit("PNG frame"));
            }
            let mut frame = vec![0; length];
            let output = reader
                .next_frame(&mut frame)
                .map_err(|e| Error::Image(e.to_string()))?;
            grayscale(&frame[..output.buffer_size()], color, &mut pixels)?;
        } else {
            while let Some(row) = reader.next_row().map_err(|e| Error::Image(e.to_string()))? {
                grayscale(row.data(), color, &mut pixels)?;
                if pixels.len() > MAX_IMAGE_PIXELS {
                    return Err(Error::Limit("PNG pixels"));
                }
            }
        }
        reader.finish().map_err(|e| Error::Image(e.to_string()))?;
        if pixels.len() != size.width as usize * size.height as usize {
            return Err(Error::Image("incomplete PNG frame".into()));
        }
        Ok(GrayImage { size, pixels })
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        let mut decoder = jpeg_reader(bytes)?;
        let info = decoder
            .info()
            .ok_or_else(|| Error::Image("missing JPEG dimensions".into()))?;
        let size = Size {
            width: info.width as u32,
            height: info.height as u32,
        };
        let pixels = decoder.decode().map_err(|e| Error::Image(e.to_string()))?;
        if pixels.len() != size.width as usize * size.height as usize {
            return Err(Error::Image(
                "JPEG did not produce a grayscale frame".into(),
            ));
        }
        Ok(GrayImage { size, pixels })
    } else {
        Err(Error::Image("supported image formats: PNG and JPEG".into()))
    }
}

fn grayscale(data: &[u8], color: png::ColorType, pixels: &mut Vec<u8>) -> Result<()> {
    let channels = match color {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err(Error::Image("unexpanded PNG palette".into())),
    };
    if !data.len().is_multiple_of(channels) {
        return Err(Error::Image("incomplete PNG row".into()));
    }
    for pixel in data.chunks_exact(channels) {
        let gray = if channels <= 2 {
            pixel[0] as u32
        } else {
            (77 * pixel[0] as u32 + 150 * pixel[1] as u32 + 29 * pixel[2] as u32 + 128) >> 8
        };
        let alpha = match channels {
            2 => pixel[1] as u32,
            4 => pixel[3] as u32,
            _ => 255,
        };
        pixels.push(((gray * alpha + 255 * (255 - alpha) + 127) / 255) as u8);
    }
    Ok(())
}

impl GrayImage {
    /// Area averaging uses every source pixel during reduction and avoids
    /// dropping thin strokes in diagrams when an image is reduced to fit a page.
    pub fn resize(&self, target: Size) -> Result<Self> {
        self.size.validate()?;
        target.validate()?;
        if self.pixels.len() != self.size.width as usize * self.size.height as usize {
            return Err(Error::Image("invalid grayscale buffer".into()));
        }
        let mut pixels = Vec::with_capacity(target.width as usize * target.height as usize);
        for y in 0..target.height {
            let top = y as f64 * self.size.height as f64 / target.height as f64;
            let bottom = (y + 1) as f64 * self.size.height as f64 / target.height as f64;
            for x in 0..target.width {
                let left = x as f64 * self.size.width as f64 / target.width as f64;
                let right = (x + 1) as f64 * self.size.width as f64 / target.width as f64;
                let mut sum = 0.0;
                for sy in top.floor() as u32..(bottom.ceil() as u32).min(self.size.height) {
                    let dy = bottom.min(sy as f64 + 1.0) - top.max(sy as f64);
                    for sx in left.floor() as u32..(right.ceil() as u32).min(self.size.width) {
                        let dx = right.min(sx as f64 + 1.0) - left.max(sx as f64);
                        sum += self.pixels[(sy * self.size.width + sx) as usize] as f64 * dx * dy;
                    }
                }
                pixels.push(
                    (sum / ((right - left) * (bottom - top)))
                        .round()
                        .clamp(0.0, 255.0) as u8,
                );
            }
        }
        Ok(Self {
            size: target,
            pixels,
        })
    }
}

/// Ordered dithering produces stable pixels across partial display refreshes.
/// Values are grayscale bytes; device-specific bit polarity belongs to its driver.
pub fn quantize(pixels: &mut [u8], width: u32, levels: u16) -> Result<()> {
    if width == 0 || !pixels.len().is_multiple_of(width as usize) {
        return Err(Error::Layout("framebuffer stride"));
    }
    if levels == 256 {
        return Ok(());
    }
    if !matches!(levels, 2 | 4) {
        return Err(Error::Layout("display levels must be 2, 4, or 256"));
    }
    const BAYER: [[u16; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    for (i, pixel) in pixels.iter_mut().enumerate() {
        let scaled = *pixel as u16 * (levels - 1);
        let threshold = 2 * BAYER[(i / width as usize) % 4][i % width as usize % 4] + 1;
        let value =
            (scaled / 255 + u16::from((scaled % 255) * 32 > threshold * 255)).min(levels - 1);
        *pixel = (value * 255 / (levels - 1)) as u8;
    }
    Ok(())
}

/// Packs one-bit rows, most significant bit first, with white padding bits.
pub fn pack_mono(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    if width == 0 || pixels.len() as u64 != width as u64 * height as u64 {
        return Err(Error::Layout("framebuffer dimensions"));
    }
    let stride = (width as usize).div_ceil(8);
    let mut output = vec![255; stride * height as usize];
    for (i, pixel) in pixels.iter().enumerate() {
        if *pixel < 128 {
            let x = i % width as usize;
            let y = i / width as usize;
            output[y * stride + x / 8] &= !(0x80 >> (x % 8));
        }
    }
    Ok(output)
}
