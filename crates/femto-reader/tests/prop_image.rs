//! Discovery properties for image size arithmetic, reduction, and decoding
//! of damaged files.
use femto_reader::image::{self, GrayImage, Limits, MAX_DIMENSION, MAX_IMAGE_PIXELS, Size};
use proptest::prelude::*;
use std::{fs, path::PathBuf};

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name),
    )
    .unwrap()
}
/// Dimensions at and around every threshold, and ordinary ones.
fn dimension() -> impl Strategy<Value = u32> {
    prop_oneof![
        4 => 1u32..=MAX_DIMENSION,
        2 => 1u32..40,
        1 => prop::sample::select(vec![0, 1, 2, 1023, 1024, 1025, MAX_DIMENSION - 1, MAX_DIMENSION, MAX_DIMENSION + 1, u32::MAX]),
    ]
}
fn side() -> impl Strategy<Value = u32> {
    prop_oneof![
        4 => 1u32..2000,
        1 => prop::sample::select(vec![0, 1, 2, 479, 480, 800, MAX_DIMENSION, u32::MAX - 1, u32::MAX]),
    ]
}
/// The size the box allows, in exact integer arithmetic: the largest size of
/// the same shape that fits, each side rounded down but not to nothing.
fn exact(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    let (w, h, mw, mh) = (
        u128::from(width),
        u128::from(height),
        u128::from(max_width),
        u128::from(max_height),
    );
    if w <= mw && h <= mh {
        (width, height)
    } else if mw * h <= mh * w {
        (max_width, ((h * mw / w) as u32).max(1))
    } else {
        (((w * mh / h) as u32).max(1), max_height)
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20_000))]

    /// A reduced size fits its box, is never larger than the source, never
    /// empty, and keeps the shape to within a pixel.
    #[test]
    fn reduction_fits_the_box_and_keeps_the_shape(width in dimension(), height in dimension(), max_width in side(), max_height in side()) {
        let size = Size { width, height };
        let valid = (1..=MAX_DIMENSION).contains(&width) && (1..=MAX_DIMENSION).contains(&height);
        let result = size.reduced(max_width, max_height);
        if !valid || max_width == 0 || max_height == 0 {
            prop_assert!(result.is_err());
            prop_assert!(size.fit(max_width, max_height).is_err());
            return Ok(());
        }
        let reduced = result.unwrap();
        prop_assert!(reduced.width >= 1 && reduced.height >= 1);
        prop_assert!(reduced.width <= max_width.min(width) && reduced.height <= max_height.min(height));
        // Within a pixel of the exact size on each side.
        let (exact_width, exact_height) = exact(width, height, max_width, max_height);
        prop_assert!(exact_width.abs_diff(reduced.width) <= 1 && exact_height.abs_diff(reduced.height) <= 1,
            "{}x{} in {}x{}: {:?}, exactly {}x{}", width, height, max_width, max_height, reduced, exact_width, exact_height);
        // A page image is the same, when it is small enough to be one.
        let page = size.fit(max_width, max_height);
        if u64::from(width) * u64::from(height) <= MAX_IMAGE_PIXELS as u64 {
            prop_assert_eq!(page.unwrap(), reduced);
        } else {
            prop_assert!(page.is_err());
        }
    }

    /// The reduced size is the exact one: an image that fills the width of
    /// its box is not drawn a pixel narrower.
    #[test]
    fn reduction_is_exact(width in dimension(), height in dimension(), max_width in side(), max_height in side()) {
        if let Ok(reduced) = (Size { width, height }).reduced(max_width, max_height) {
            prop_assert_eq!((reduced.width, reduced.height), exact(width, height, max_width, max_height),
                "{}x{} in {}x{}", width, height, max_width, max_height);
        }
    }

    /// Area averaging keeps the size asked for, a flat image flat, the range
    /// of the source, and its mean.
    #[test]
    fn resizing_averages(
        (width, height, pixels) in (1u32..40, 1u32..40).prop_flat_map(|(w, h)| (Just(w), Just(h), prop_oneof![
            prop::collection::vec(any::<u8>(), (w * h) as usize),
            any::<u8>().prop_map(move |value| vec![value; (w * h) as usize]),
            prop::collection::vec(prop::sample::select(vec![0u8, 255]), (w * h) as usize),
        ])),
        target_width in 1u32..40, target_height in 1u32..40,
    ) {
        let source = GrayImage { size: Size { width, height }, pixels: pixels.clone() };
        let target = Size { width: target_width.min(width), height: target_height.min(height) };
        let resized = source.resize(target).unwrap();
        prop_assert_eq!(resized.size, target);
        prop_assert_eq!(resized.pixels.len(), (target.width * target.height) as usize);
        let (low, high) = (*pixels.iter().min().unwrap(), *pixels.iter().max().unwrap());
        prop_assert!(resized.pixels.iter().all(|p| (low..=high).contains(p)));
        let mean = |pixels: &[u8]| pixels.iter().map(|p| f64::from(*p)).sum::<f64>() / pixels.len() as f64;
        prop_assert!((mean(&pixels) - mean(&resized.pixels)).abs() <= 0.5 + 1e-9, "{} became {}", mean(&pixels), mean(&resized.pixels));
        if target == source.size {
            prop_assert_eq!(&resized.pixels, &pixels);
        }
        // A whole-number reduction is the rounded mean of each cell.
        if width % target.width == 0 && height % target.height == 0 {
            let (fx, fy) = (width / target.width, height / target.height);
            for y in 0..target.height {
                for x in 0..target.width {
                    let sum: u32 = (0..fy).flat_map(|dy| (0..fx).map(move |dx| (dx, dy)))
                        .map(|(dx, dy)| u32::from(pixels[((y * fy + dy) * width + x * fx + dx) as usize])).sum();
                    let cell = fx * fy;
                    prop_assert_eq!(u32::from(resized.pixels[(y * target.width + x) as usize]), (sum + cell / 2) / cell);
                }
            }
        }
    }

    #[test]
    fn resizing_rejects_what_it_cannot_do(width in 0u32..6, height in 0u32..6, extra in 0usize..3, target_width in dimension(), target_height in dimension()) {
        let source = GrayImage { size: Size { width, height }, pixels: vec![7; (width * height) as usize + extra] };
        let target = Size { width: target_width, height: target_height };
        let possible = width > 0 && height > 0 && extra == 0 && target_width > 0 && target_height > 0
            && target_width <= MAX_DIMENSION && target_height <= MAX_DIMENSION
            && u64::from(target_width) * u64::from(target_height) <= MAX_IMAGE_PIXELS as u64;
        match source.resize(target) {
            Ok(resized) => {
                prop_assert!(possible);
                prop_assert!(resized.pixels.iter().all(|p| *p == 7));
            }
            Err(_) => prop_assert!(!possible),
        }
    }
}

fn check_decoded(bytes: &[u8]) -> Result<(), TestCaseError> {
    for limits in [Limits::PAGE, Limits::ALONE] {
        if let Ok(decoded) = image::decode_within(bytes, limits) {
            let count = decoded.size.width as usize * decoded.size.height as usize;
            prop_assert_eq!(decoded.pixels.len(), count);
            prop_assert!(count <= limits.pixels && count > 0);
            prop_assert_eq!(
                image::dimensions(bytes)
                    .ok()
                    .filter(|_| count <= Limits::PAGE.pixels),
                Some(decoded.size).filter(|_| count <= Limits::PAGE.pixels)
            );
        }
    }
    let _ = image::dimensions(bytes);
    Ok(())
}
const FIXTURES: [&str; 10] = [
    "alpha.png",
    "diagram.png",
    "gray-alpha.png",
    "interlaced.png",
    "palette.png",
    "sixteen.png",
    "tall.png",
    "oversized.png",
    "gradient.jpg",
    "progressive.jpg",
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// A damaged image is an error or an image of the size it says, within
    /// the limits; never a panic.
    #[test]
    fn damaged_images_never_panic(
        name in prop::sample::select(FIXTURES.to_vec()),
        edits in prop::collection::vec((any::<prop::sample::Index>(), prop_oneof![any::<u8>(), Just(0), Just(0xff)], 0u8..3), 0..4),
        cut in prop::option::of(any::<prop::sample::Index>()),
    ) {
        let mut bytes = fixture(name);
        for (index, byte, kind) in edits {
            let at = index.index(bytes.len());
            match kind {
                0 => bytes[at] = byte,
                1 => bytes.insert(at, byte),
                _ => { bytes.remove(at); }
            }
        }
        if let Some(cut) = cut {
            bytes.truncate(cut.index(bytes.len() + 1));
        }
        check_decoded(&bytes)?;
    }

    /// Header fields set to extremes: sizes, depths and counts.
    #[test]
    fn extreme_header_fields_never_panic(
        name in prop::sample::select(FIXTURES.to_vec()),
        at in 0usize..200, value in prop::sample::select(vec![0u32, 1, 0x7fff_ffff, 0x8000_0000, u32::MAX, 8192, 8193, 0x0001_0000, 0x2000_2000]), width in 1usize..=4,
    ) {
        let mut bytes = fixture(name);
        let at = at.min(bytes.len() - 4);
        bytes[at..at + width].copy_from_slice(&value.to_be_bytes()[4 - width..]);
        check_decoded(&bytes)?;
    }

    #[test]
    fn arbitrary_bytes_never_panic(
        magic in prop::sample::select(vec![&b"\x89PNG\r\n\x1a\n"[..], &[0xff, 0xd8], &[0xff, 0xd8, 0xff, 0xc2], &[0xff, 0xd8, 0xff, 0xe0, 0, 2, 0xff, 0xc0], b""]),
        bytes in prop::collection::vec(prop_oneof![any::<u8>(), Just(0xff)], 0..200),
    ) {
        check_decoded(&[magic, &bytes].concat())?;
    }
}

/// Every truncation of every fixture, and every single-byte change to the
/// small ones.
#[test]
fn every_truncation_of_every_fixture_is_handled() {
    for name in FIXTURES {
        let whole = fixture(name);
        let step = (whole.len() / 400).max(1);
        for cut in (0..whole.len()).step_by(step) {
            check_decoded(&whole[..cut]).unwrap();
        }
        if whole.len() < 200 {
            for at in 0..whole.len() {
                for value in [0, 1, 0x7f, 0x80, 0xff, whole[at] ^ 1] {
                    let mut bytes = whole.clone();
                    bytes[at] = value;
                    check_decoded(&bytes).unwrap();
                }
            }
        }
    }
}

/// "The size reduced to fit a box": the height limits a 1 x 2931 image in a
/// 1 x 107 box, and 2931 * (107 / 2931) is just under 107 in floating point.
#[test]
fn an_image_limited_by_height_fills_the_height() {
    for (width, height, max_width, max_height, expected) in [
        (1, 2931, 1, 107, (1, 107)),
        (2931, 1, 107, 1, (107, 1)),
        (2931, 2931, 107, 500, (107, 107)),
    ] {
        let reduced = Size { width, height }
            .reduced(max_width, max_height)
            .unwrap();
        assert_eq!(
            (reduced.width, reduced.height),
            expected,
            "{width}x{height} in {max_width}x{max_height}"
        );
    }
}

/// docs/development.md and `Limits::ALONE`: a progressive JPEG over one
/// megapixel is refused, because it needs memory for every coefficient at
/// once. The frame type is found by walking the segments; bytes a decoder
/// skips between segments must not hide it.
#[test]
fn a_progressive_jpeg_is_recognized_however_its_segments_are_padded() {
    let whole = fixture("large-progressive.jpg");
    assert!(image::decode_within(&whole, Limits::ALONE).is_err());
    let frame = whole
        .windows(2)
        .position(|pair| pair == [0xff, 0xc2])
        .unwrap();
    let mut accepted = Vec::new();
    for (label, padding) in [
        ("fill byte", &[0xff][..]),
        ("fill bytes", &[0xff, 0xff, 0xff]),
        ("stray byte", &[0x00]),
        ("stray bytes", &[0x12, 0x34]),
        ("restart marker", &[0xff, 0xd0]),
        ("second start of image", &[0xff, 0xd8]),
        ("TEM marker", &[0xff, 0x01]),
    ] {
        for at in [2, frame] {
            let mut bytes = whole.clone();
            bytes.splice(at..at, padding.iter().copied());
            if let Ok(decoded) = image::decode_within(&bytes, Limits::ALONE) {
                accepted.push((label, at, decoded.size));
            }
        }
    }
    assert!(
        accepted.is_empty(),
        "decoded as if not progressive: {accepted:?}"
    );
}
