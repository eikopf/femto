use femto_reader::{
    display::TextRaster,
    document::Document,
    layout::{Font, Fonts, LayoutConfig, paginate, rasterize_display},
};
use std::path::PathBuf;

#[test]
fn coverage_controls_preserve_endpoints_and_reject_invalid_profiles() {
    let neutral = TextRaster::default().coverage_table().unwrap();
    assert_eq!(neutral, std::array::from_fn(|i| i as u8));
    let darker = TextRaster {
        coverage_gamma: 0.7,
        ..Default::default()
    }
    .coverage_table()
    .unwrap();
    assert_eq!((darker[0], darker[255]), (0, 255));
    assert!(darker.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(darker[128] > neutral[128]);
    assert!(
        TextRaster {
            stroke: f32::NAN,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        TextRaster {
            coverage_gamma: 0.0,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
}

#[test]
fn raster_controls_change_ink_without_changing_layout_and_use_native_levels() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut document = Document::open(root.join("sample.epub")).unwrap();
    let fonts = Fonts::new(Font::load(root.join("DejaVuSerif.ttf")).unwrap());
    let config = LayoutConfig::default();
    let pages = paginate(&document.chapter(0).unwrap(), &fonts, config).unwrap();
    let page = &pages.pages[0];
    let render = |text, levels| {
        rasterize_display(page, &fonts, config, text, levels, |_| panic!("text page")).unwrap()
    };
    let baseline = render(TextRaster::default(), 256);
    let darker = render(
        TextRaster {
            coverage_gamma: 0.7,
            ..Default::default()
        },
        256,
    );
    let heavier = render(
        TextRaster {
            stroke: 0.2,
            ..Default::default()
        },
        256,
    );
    let ink = |pixels: &[u8]| pixels.iter().map(|v| 255 - *v as u64).sum::<u64>();
    assert!(ink(&darker) > ink(&baseline));
    assert!(ink(&heavier) > ink(&baseline));
    let fractional = render(
        TextRaster {
            fractional: true,
            ..Default::default()
        },
        256,
    );
    assert_ne!(fractional, baseline);
    for levels in [2, 4] {
        let pixels = render(TextRaster::default(), levels);
        assert!(
            pixels
                .iter()
                .all(|v| (*v as u16).is_multiple_of(255 / (levels - 1)))
        );
        // Nearest-level text quantization has no dependence on a Bayer grid.
        for (source, result) in baseline.iter().zip(&pixels) {
            let steps = levels - 1;
            assert_eq!(
                *result as u16,
                ((*source as u16 * steps + 127) / 255) * 255 / steps
            );
        }
    }
}
