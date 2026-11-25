use std::{fs, path::PathBuf};

use femto_reader::{
    document::Document,
    image::{self, GrayImage, Size},
    layout::{Font, Fonts, LayoutConfig, paginate, rasterize_with_assets},
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

#[test]
fn png_alpha_palette_depth_and_interlace() {
    for name in ["alpha.png", "palette.png", "gray-alpha.png"] {
        let decoded = image::decode(&fs::read(fixture(name)).unwrap()).unwrap();
        assert_eq!(
            decoded.size,
            Size {
                width: 3,
                height: 1
            }
        );
        assert_eq!(decoded.pixels[0], 255);
        assert_eq!(decoded.pixels[1], 127);
        assert_eq!(
            decoded.pixels[2],
            if name == "gray-alpha.png" { 255 } else { 77 }
        );
    }
    assert_eq!(
        image::decode(&fs::read(fixture("sixteen.png")).unwrap())
            .unwrap()
            .pixels,
        [0, 128, 255]
    );
    let interlaced = image::decode(&fs::read(fixture("interlaced.png")).unwrap()).unwrap();
    let expected: Vec<u8> = (0..8)
        .flat_map(|y| (0..8).map(move |x| x * 16 + y))
        .collect();
    assert_eq!(interlaced.pixels, expected);
}

#[test]
fn baseline_and_progressive_jpeg_decode_to_luma() {
    for name in ["gradient.jpg", "progressive.jpg"] {
        let data = fs::read(fixture(name)).unwrap();
        let decoded = image::decode(&data).unwrap();
        assert_eq!(
            decoded.size,
            Size {
                width: 320,
                height: 240
            }
        );
        assert_eq!(decoded.pixels.len(), 320 * 240);
        assert!(decoded.pixels[0] < 4);
        assert!(*decoded.pixels.last().unwrap() > 250);
        assert_eq!(image::dimensions(&data).unwrap(), decoded.size);
    }
}

#[test]
fn invalid_and_oversized_images_are_rejected() {
    assert!(image::dimensions(&fs::read(fixture("oversized.png")).unwrap()).is_err());
    assert!(image::decode(&vec![0; image::MAX_ENCODED_BYTES + 1]).is_err());
    let valid = fs::read(fixture("alpha.png")).unwrap();
    for length in [0, 8, 24, valid.len() - 12] {
        assert!(image::decode(&valid[..length]).is_err());
    }
    let mut corrupt = valid.clone();
    corrupt[44] ^= 1;
    assert!(image::decode(&corrupt).is_err());
    assert!(image::decode(b"<svg/>").is_err());
}

#[test]
fn reduction_preserves_average_and_alpha_composites_on_white() {
    let original = GrayImage {
        size: Size {
            width: 2,
            height: 2,
        },
        pixels: vec![0, 255, 255, 0],
    };
    assert_eq!(
        original
            .resize(Size {
                width: 1,
                height: 1
            })
            .unwrap()
            .pixels,
        [128]
    );
    let unchanged = original.resize(original.size).unwrap();
    assert_eq!(unchanged.pixels, original.pixels);
    assert_eq!(
        Size {
            width: 120,
            height: 1000
        }
        .fit(432, 752)
        .unwrap(),
        Size {
            width: 90,
            height: 752
        }
    );
}

/// A box side that limits an image is filled: no pixel is lost to rounding.
#[test]
fn a_reduced_image_fills_the_side_of_the_box_that_limits_it() {
    for (width, height, max_width, max_height, expected) in [
        (1, 2931, 1, 107, (1, 107)),
        (2931, 1, 107, 1, (107, 1)),
        (2931, 2931, 107, 500, (107, 107)),
        // The other side is rounded down, but not to nothing.
        (1400, 2100, 480, 800, (480, 720)),
        (3, 2, 2, 2, (2, 1)),
        (8192, 1, 100, 100, (100, 1)),
        // Never enlarged, however large the box.
        (300, 200, 600, u32::MAX, (300, 200)),
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

#[test]
fn dither_levels_and_packed_row_boundaries() {
    for levels in [2, 4] {
        let mut pixels = vec![128; 16];
        image::quantize(&mut pixels, 4, levels).unwrap();
        assert!(pixels.iter().all(|v| if levels == 2 {
            matches!(v, 0 | 255)
        } else {
            matches!(v, 0 | 85 | 170 | 255)
        }));
        let mean = pixels.iter().map(|v| *v as u32).sum::<u32>() / 16;
        assert!((120..=136).contains(&mean));
    }
    let mut extremes = vec![0, 255, 0, 255];
    image::quantize(&mut extremes, 2, 2).unwrap();
    assert_eq!(extremes, [0, 255, 0, 255]);
    let mut pixels = vec![255; 18];
    pixels[0] = 0;
    pixels[8] = 0;
    pixels[10] = 0;
    assert_eq!(
        image::pack_mono(&pixels, 9, 2).unwrap(),
        [0x7f, 0x7f, 0xbf, 0xff]
    );
    assert!(image::quantize(&mut pixels, 9, 3).is_err());
}

#[test]
fn epub_images_place_render_and_resume_without_loading_a_whole_book() {
    let mut doc = Document::open(fixture("images.epub")).unwrap();
    let chapter = doc.chapter(0).unwrap();
    assert_eq!(chapter.warnings.len(), 1);
    let fonts = Fonts::new(Font::load(fixture("DejaVuSerif.ttf")).unwrap());
    let config = LayoutConfig::default();
    let pages = paginate(&chapter, &fonts, config).unwrap();
    assert_eq!(
        pages
            .pages
            .iter()
            .map(|page| page.images.len())
            .sum::<usize>(),
        3
    );
    assert!(
        pages
            .pages
            .iter()
            .any(|page| !page.images.is_empty() && page.lines.is_empty())
    );
    let rendered_text = pages
        .pages
        .iter()
        .flat_map(|page| &page.lines)
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        rendered_text.contains("Missing illustration"),
        "{rendered_text}"
    );
    let mut resources = Vec::new();
    for (index, page) in pages.pages.iter().enumerate() {
        assert_eq!(pages.page_for(page.start), index);
        for placement in &page.images {
            assert!(placement.x >= config.margin && placement.y >= config.margin);
            assert!(placement.x + placement.size.width <= config.width - config.margin);
            assert!(placement.y + placement.size.height <= config.height - config.margin);
        }
        let pixels = rasterize_with_assets(page, &fonts, config, |source| {
            resources.push(source.to_string());
            doc.resource(source)
        })
        .unwrap();
        assert_eq!(pixels.len(), 480 * 800);
        for placement in &page.images {
            let region_ink = (placement.y..placement.y + placement.size.height)
                .flat_map(|y| {
                    (placement.x..placement.x + placement.size.width)
                        .map(move |x| (y * 480 + x) as usize)
                })
                .filter(|i| pixels[*i] < 240)
                .count();
            assert!(region_ink > 100);
        }
    }
    assert_eq!(resources.len(), 3);
    assert!(resources.iter().all(|source| source.starts_with("images/")));
    assert!(doc.resource("https://example.org/a.png").is_err());
    assert!(doc.resource("/etc/passwd").is_err());
}

#[test]
fn corrupt_pixels_preserve_layout_text_and_reading_navigation() {
    use femto_reader::{
        controller::{Action, Reader},
        state::ReaderState,
    };
    let fonts = Fonts::new(Font::load(fixture("DejaVuSerif.ttf")).unwrap());
    let mut valid = Document::open(fixture("images.epub")).unwrap();
    let expected = paginate(&valid.chapter(0).unwrap(), &fonts, LayoutConfig::default()).unwrap();
    let mut reader = Reader::new(
        Document::open(fixture("corrupt-images.epub")).unwrap(),
        fonts,
        ReaderState::default(),
    )
    .unwrap();
    assert_eq!(reader.page_count(), expected.pages.len());
    let before = reader.warnings().len();
    for (index, page) in expected.pages.iter().enumerate() {
        assert_eq!(reader.page_index(), index);
        assert_eq!(reader.page().start, page.start);
        assert_eq!(reader.page().end, page.end);
        assert_eq!(
            reader
                .page()
                .lines
                .iter()
                .map(|l| &l.text)
                .collect::<Vec<_>>(),
            page.lines.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
        let frame = reader.render(2).unwrap();
        assert_eq!(reader.render(2).unwrap(), frame);
        assert!(frame.iter().all(|v| matches!(v, 0 | 255)));
        reader.dispatch(Action::Next).unwrap();
    }
    assert_eq!(reader.warnings().len(), before + 1);
    assert!(reader.warnings().iter().any(|w| w.contains("diagram.png")));
    reader
        .open_document(Document::open(fixture("corrupt-images.epub")).unwrap())
        .unwrap();
    reader.render(4).unwrap();
}

#[test]
fn asset_failures_are_reported_but_invalid_geometry_still_fails() {
    use femto_reader::{
        Error,
        layout::{Locator, Page, PlacedImage, rasterize_display_report},
    };
    let fonts = Fonts::new(Font::load(fixture("DejaVuSerif.ttf")).unwrap());
    let config = LayoutConfig::default();
    let mut page = Page {
        start: Locator::default(),
        end: Locator::default(),
        lines: vec![],
        images: vec![PlacedImage {
            source: "missing.png".into(),
            x: 24,
            y: 24,
            size: Size {
                width: 100,
                height: 80,
            },
            location: Locator::default(),
        }],
    };
    let render = |page: &Page| {
        rasterize_display_report(page, &fonts, config, Default::default(), 4, |_| {
            Err(Error::Image("unavailable".into()))
        })
    };
    let report = render(&page).unwrap();
    assert_eq!(report.warnings.len(), 1);
    assert!(report.warnings[0].contains("missing.png"));
    for y in 0..config.height {
        for x in 0..config.width {
            let value = report.pixels[(y * config.width + x) as usize];
            if !(24..124).contains(&x) || !(24..104).contains(&y) {
                assert_eq!(value, 255);
            }
        }
    }
    assert_eq!(report.pixels[24 * 480 + 24], 0);
    assert_eq!(report.pixels[103 * 480 + 123], 0);
    page.images[0].x = u32::MAX;
    assert!(render(&page).is_err());
    page.images[0].x = 24;
    page.images[0].size.width = 0;
    assert!(render(&page).is_err());
    page.images[0].size.width = 1;
    page.images[0].size.height = 1;
    assert!(render(&page).is_ok());
    page.images = vec![page.images[0].clone(); 100];
    assert_eq!(render(&page).unwrap().warnings.len(), 64);
}

#[test]
fn an_image_decoded_alone_may_be_larger_unless_it_is_progressive() {
    let baseline = fs::read(fixture("large-baseline.jpg")).unwrap();
    let progressive = fs::read(fixture("large-progressive.jpg")).unwrap();
    // 1400x2100 is more than a page image may be.
    assert!(image::decode(&baseline).is_err());
    assert!(image::dimensions(&baseline).is_err());
    let decoded = image::decode_within(&baseline, image::Limits::ALONE).unwrap();
    assert_eq!(
        decoded.size,
        Size {
            width: 1400,
            height: 2100
        }
    );
    // The gradient runs from black at the top to white at the bottom.
    assert!(decoded.pixels[0] < 8 && *decoded.pixels.last().unwrap() > 247);
    let reduced = decoded
        .resize(decoded.size.reduced(480, 800).unwrap())
        .unwrap();
    assert_eq!(
        reduced.size,
        Size {
            width: 480,
            height: 720
        }
    );
    assert!(decoded.size.fit(480, 800).is_err());
    // A progressive image of that size needs its coefficients all at once.
    assert!(image::decode_within(&progressive, image::Limits::ALONE).is_err());
    assert!(
        image::decode_within(
            &fs::read(fixture("progressive.jpg")).unwrap(),
            image::Limits::ALONE
        )
        .is_ok()
    );
}
