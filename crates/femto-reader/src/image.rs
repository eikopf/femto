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

/// How large an image may be, encoded and decoded.
#[derive(Clone, Copy)]
pub struct Limits {
    pub encoded: usize,
    pub pixels: usize,
}
impl Limits {
    /// An image on a page, decoded beside everything a page needs.
    pub const PAGE: Self = Self {
        encoded: MAX_ENCODED_BYTES,
        pixels: MAX_IMAGE_PIXELS,
    };
    /// An image decoded when little else is, such as the cover behind the
    /// sleep page. Covers are often two to four megapixels. The decoded
    /// pixels are allocated fallibly, so an image the device has no room
    /// for is refused and not fatal. The archive reader caps every entry at
    /// the same encoded size as a page image.
    pub const ALONE: Self = Self {
        encoded: MAX_ENCODED_BYTES,
        pixels: 4 * 1024 * 1024,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    fn validate(self) -> Result<Self> {
        self.within(MAX_IMAGE_PIXELS)
    }
    fn within(self, pixels: usize) -> Result<Self> {
        if self.width == 0
            || self.height == 0
            || self.width > MAX_DIMENSION
            || self.height > MAX_DIMENSION
            || self.width as u64 * self.height as u64 > pixels as u64
        {
            return Err(Error::Limit("image dimensions"));
        }
        Ok(self)
    }

    /// The size reduced to fit a box, for an image a page may hold.
    pub fn fit(self, max_width: u32, max_height: u32) -> Result<Self> {
        self.validate()?;
        self.reduced(max_width, max_height)
    }
    /// The size reduced to fit a box; never enlarged.
    pub fn reduced(self, max_width: u32, max_height: u32) -> Result<Self> {
        self.within(usize::MAX)?;
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

fn encoded(bytes: &[u8], limits: Limits) -> Result<()> {
    if bytes.len() > limits.encoded {
        return Err(Error::Limit("encoded image"));
    }
    Ok(())
}

/// A buffer for decoded pixels, refused when there is no memory for it or
/// too little would be left beside it. What follows allocates infallibly:
/// the decoder's working rows, and a copy reduced to the size of a screen.
fn buffer(capacity: usize) -> Result<Vec<u8>> {
    const BESIDE: usize = 768 * 1024;
    let reserve = |bytes| {
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(bytes)
            .map(|()| buffer)
            .map_err(|_| Error::Limit("image memory"))
    };
    let pixels = reserve(capacity)?;
    reserve(BESIDE)?;
    Ok(pixels)
}

/// Whether a JPEG is progressive, which takes memory for every coefficient
/// of the image at once and not only for its pixels.
fn progressive(bytes: &[u8]) -> bool {
    let mut at = 2;
    while let Some(&[0xff, marker, high, low]) = bytes
        .get(at..at + 4)
        .and_then(|header| <&[u8; 4]>::try_from(header).ok())
    {
        match marker {
            0xc2 | 0xc6 | 0xca | 0xce => return true,
            // Another frame type, or the scan: the frame header has passed.
            0xc0 | 0xc1 | 0xc3 | 0xc5 | 0xc7 | 0xc9 | 0xcb | 0xcd | 0xcf | 0xda => return false,
            _ => at += 2 + usize::from(u16::from_be_bytes([high, low])),
        }
    }
    false
}

fn png_reader(bytes: &[u8], limits: Limits) -> Result<png::Reader<Cursor<&[u8]>>> {
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
    .within(limits.pixels)?;
    Ok(reader)
}

/// The decoder holds tens of kilobytes of tables. It stays on the heap so
/// that callers do not each reserve a copy on a small device stack.
fn jpeg_reader(bytes: &[u8], limits: Limits) -> Result<Box<JpegDecoder<ZCursor<&[u8]>>>> {
    let options = DecoderOptions::default()
        .set_max_width(MAX_DIMENSION as usize)
        .set_max_height(MAX_DIMENSION as usize)
        .jpeg_set_out_colorspace(ColorSpace::Luma);
    let mut decoder = Box::new(JpegDecoder::new_with_options(ZCursor::new(bytes), options));
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
    .within(if progressive(bytes) {
        limits.pixels.min(MAX_IMAGE_PIXELS)
    } else {
        limits.pixels
    })?;
    Ok(decoder)
}

pub fn dimensions(bytes: &[u8]) -> Result<Size> {
    encoded(bytes, Limits::PAGE)?;
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let reader = png_reader(bytes, Limits::PAGE)?;
        Ok(Size {
            width: reader.info().width,
            height: reader.info().height,
        })
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        let decoder = jpeg_reader(bytes, Limits::PAGE)?;
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
    decode_within(bytes, Limits::PAGE)
}

pub fn decode_within(bytes: &[u8], limits: Limits) -> Result<GrayImage> {
    encoded(bytes, limits)?;
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let mut reader = png_reader(bytes, limits)?;
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
        let mut pixels = buffer(size.width as usize * size.height as usize)?;
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
                if pixels.len() > limits.pixels {
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
        let mut decoder = jpeg_reader(bytes, limits)?;
        let info = decoder
            .info()
            .ok_or_else(|| Error::Image("missing JPEG dimensions".into()))?;
        let size = Size {
            width: info.width as u32,
            height: info.height as u32,
        };
        if decoder.output_buffer_size() != Some(size.width as usize * size.height as usize) {
            return Err(Error::Image(
                "JPEG did not produce a grayscale frame".into(),
            ));
        }
        let mut pixels = buffer(size.width as usize * size.height as usize)?;
        pixels.resize(pixels.capacity(), 0);
        decoder
            .decode_into(&mut pixels)
            .map_err(|e| Error::Image(e.to_string()))?;
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
        self.size.within(usize::MAX)?;
        target.validate()?;
        if self.pixels.len() != self.size.width as usize * self.size.height as usize {
            return Err(Error::Image("invalid grayscale buffer".into()));
        }
        // Exact integer weights: along each axis a source pixel spans `target`
        // units and a target pixel spans `source` units of a common grid.
        // Devices without double-precision hardware make float weights
        // thousands of times slower.
        let spans = |source: u32, target: u32| -> Vec<(u32, u32, u64, u64)> {
            (0..u64::from(target))
                .map(|index| {
                    let (start, end) = (index * u64::from(source), (index + 1) * u64::from(source));
                    let first = (start / u64::from(target)) as u32;
                    let last = end.div_ceil(u64::from(target)) as u32;
                    (first, last, start, end)
                })
                .collect()
        };
        let overlap = |pixel: u32, unit: u32, start: u64, end: u64| {
            let (low, high) = (
                u64::from(pixel) * u64::from(unit),
                (u64::from(pixel) + 1) * u64::from(unit),
            );
            high.min(end) - low.max(start)
        };
        let columns = spans(self.size.width, target.width);
        let area = u64::from(self.size.width) * u64::from(self.size.height);
        let mut pixels = Vec::with_capacity(target.width as usize * target.height as usize);
        for (top, bottom, y_start, y_end) in spans(self.size.height, target.height) {
            for &(left, right, x_start, x_end) in &columns {
                let mut sum = 0;
                for sy in top..bottom {
                    let row =
                        &self.pixels[(sy * self.size.width) as usize..][..self.size.width as usize];
                    let mut line = 0;
                    for sx in left..right {
                        line +=
                            u64::from(row[sx as usize]) * overlap(sx, target.width, x_start, x_end);
                    }
                    sum += line * overlap(sy, target.height, y_start, y_end);
                }
                pixels.push(((sum + area / 2) / area) as u8);
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
