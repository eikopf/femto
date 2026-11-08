use epub_stream::css::ImageLimit;
use femto_reader::{
    Error,
    document::{Block, Document},
    layout::{Font, Fonts, LayoutConfig, paginate, rasterize_with_assets},
};

fn fonts() -> Fonts {
    Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    )
}

fn document() -> Document {
    Document::open(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/image-limits.epub"),
    )
    .unwrap()
}

#[test]
fn cascaded_image_constraints_reflow_against_the_content_box() {
    let mut document = document();
    let chapter = document.chapter(0).unwrap();
    let fonts = fonts();
    for (width, height, expected) in [
        (
            240,
            300,
            vec![
                (200, 150),
                (100, 75),
                (40, 30),
                (86, 65),
                (200, 150),
                (33, 24),
                (200, 150),
                (80, 60),
            ],
        ),
        (
            440,
            500,
            vec![
                (266, 200),
                (200, 150),
                (40, 30),
                (153, 115),
                (266, 200),
                (33, 24),
                (320, 240),
                (80, 60),
            ],
        ),
    ] {
        let config = LayoutConfig {
            width,
            height,
            margin: 20,
            ..Default::default()
        };
        let pages = paginate(&chapter, &fonts, config).unwrap();
        let images: Vec<_> = pages.pages.iter().flat_map(|page| &page.images).collect();
        assert_eq!(
            images
                .iter()
                .map(|image| (image.size.width, image.size.height))
                .collect::<Vec<_>>(),
            expected
        );
        for image in images {
            assert_eq!(image.x, (width - image.size.width) / 2);
            assert!(image.y + image.size.height <= height - config.margin);
            let Block::Image(source) = &chapter.blocks[image.location.block] else {
                panic!()
            };
            assert!(!source.alt.starts_with("Hidden"));
        }
        assert!(
            pages
                .pages
                .iter()
                .flat_map(|page| &page.lines)
                .any(|line| line.text.contains("Reading"))
        );
    }
}

#[test]
fn zero_limits_allow_an_empty_image_only_chapter_without_decode_or_placeholder() {
    let mut document = document();
    let mut chapter = document.chapter(0).unwrap();
    chapter
        .blocks
        .retain(|block| matches!(block, Block::Image(image) if image.alt.starts_with("Hidden")));
    let fonts = fonts();
    let config = LayoutConfig::default();
    let pages = paginate(&chapter, &fonts, config).unwrap();
    assert_eq!(pages.pages.len(), 1);
    assert!(pages.pages[0].images.is_empty());
    assert!(pages.pages[0].lines.is_empty());
    let pixels = rasterize_with_assets(&pages.pages[0], &fonts, config, |_| {
        panic!("zero-size image requested an asset during rasterization")
    })
    .unwrap();
    assert!(pixels.iter().all(|pixel| *pixel == 255));
}

#[test]
fn manually_constructed_invalid_limits_fail_without_float_cast_overflow() {
    let mut document = document();
    let mut chapter = document.chapter(0).unwrap();
    chapter
        .blocks
        .retain(|block| matches!(block, Block::Image(_)));
    chapter.blocks.truncate(1);
    let fonts = fonts();
    for invalid in [f32::NAN, f32::INFINITY, -1.0] {
        let Block::Image(image) = &mut chapter.blocks[0] else {
            panic!()
        };
        image.max_width = Some(ImageLimit::Percent(invalid));
        assert!(matches!(
            paginate(&chapter, &fonts, LayoutConfig::default()),
            Err(Error::Layout("invalid image size limit"))
        ));
    }
    let Block::Image(image) = &mut chapter.blocks[0] else {
        panic!()
    };
    image.max_width = Some(ImageLimit::Percent(f32::MAX));
    image.max_height = Some(ImageLimit::Px(f32::MAX));
    let pages = paginate(&chapter, &fonts, LayoutConfig::default()).unwrap();
    assert_eq!(
        pages.pages[0].images[0].size,
        femto_reader::image::Size {
            width: 320,
            height: 240
        }
    );
}

#[test]
fn explicit_dimensions_cascade_render_and_reconstruct_pages() {
    let mut document = Document::open(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/image-dimensions.epub"),
    )
    .unwrap();
    let chapter = document.chapter(0).unwrap();
    let fonts = fonts();
    for width in [240, 440] {
        let config = LayoutConfig {
            width,
            height: 500,
            margin: 20,
            ..Default::default()
        };
        let pages = paginate(&chapter, &fonts, config).unwrap();
        let expected = if width == 240 {
            vec![
                (100, 75),
                (40, 30),
                (80, 20),
                (200, 150),
                (100, 75),
                (53, 40),
                (40, 80),
                (200, 150),
                (200, 150),
            ]
        } else {
            vec![
                (200, 150),
                (40, 30),
                (80, 20),
                (400, 300),
                (100, 75),
                (53, 40),
                (40, 80),
                (320, 240),
                (320, 240),
            ]
        };
        assert_eq!(
            pages
                .pages
                .iter()
                .flat_map(|page| &page.images)
                .map(|image| (image.size.width, image.size.height))
                .collect::<Vec<_>>(),
            expected
        );
        for page in &pages.pages {
            let render = |page: &femto_reader::layout::Page, document: &mut Document| {
                rasterize_with_assets(page, &fonts, config, |source| {
                    assert!(!source.contains("missing"));
                    document.resource(source)
                })
                .unwrap()
            };
            let pixels = render(page, &mut document);
            assert!(pixels.iter().any(|pixel| *pixel != 255));
            femto_reader::layout::paginate_from(&chapter, &fonts, config, page.start, |rebuilt| {
                assert_eq!(rebuilt.start, page.start);
                assert_eq!(rebuilt.end, page.end);
                assert_eq!(render(&rebuilt, &mut document), pixels);
                Ok(false)
            })
            .unwrap();
        }
    }
}

#[test]
fn explicit_dimensions_validate_and_fit_before_integer_conversion() {
    let mut document = document();
    let mut chapter = document.chapter(0).unwrap();
    chapter
        .blocks
        .retain(|block| matches!(block, Block::Image(_)));
    chapter.blocks.truncate(1);
    let fonts = fonts();
    let config = LayoutConfig::default();
    for invalid in [f32::NAN, f32::INFINITY, -1.0] {
        let Block::Image(image) = &mut chapter.blocks[0] else {
            panic!()
        };
        image.width = Some(ImageLimit::Px(invalid));
        assert!(paginate(&chapter, &fonts, config).is_err());
    }
    let Block::Image(image) = &mut chapter.blocks[0] else {
        panic!()
    };
    image.width = Some(ImageLimit::Percent(f32::MAX));
    image.height = Some(ImageLimit::Px(f32::MAX));
    image.max_height = None;
    let pages = paginate(&chapter, &fonts, config).unwrap();
    let size = pages.pages[0].images[0].size;
    assert!(size.width > 0 && size.width <= config.width - 2 * config.margin);
    assert!(size.height > 0 && size.height <= config.height - 2 * config.margin);
    let large = LayoutConfig {
        width: 2048,
        height: 2048,
        ..config
    };
    let pages = paginate(&chapter, &fonts, large).unwrap();
    let size = pages.pages[0].images[0].size;
    assert!(
        u64::from(size.width) * u64::from(size.height)
            <= femto_reader::image::MAX_IMAGE_PIXELS as u64
    );
    let Block::Image(image) = &mut chapter.blocks[0] else {
        panic!()
    };
    image.width = Some(ImageLimit::Px(0.0));
    image.intrinsic = None;
    let pages = paginate(&chapter, &fonts, config).unwrap();
    assert!(pages.pages[0].images.is_empty());
    assert!(pages.pages[0].lines.is_empty());
}
