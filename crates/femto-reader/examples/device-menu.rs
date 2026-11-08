use femto_reader::{
    controller::Reader,
    document::Document,
    layout::{Font, Fonts},
    state::ReaderState,
    ui::ReaderUi,
};
use std::{
    fs::{self, File},
    path::PathBuf,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut reader = Reader::new(
        Document::open(root.join("tests/fixtures/sample.epub"))?,
        Fonts::new(Font::load(root.join("tests/fixtures/DejaVuSerif.ttf"))?),
        ReaderState::default(),
    )?;
    let mut ui = ReaderUi::default();
    let output = root.join("output/device-menu");
    fs::create_dir_all(&output)?;
    ui.open();
    for (name, next) in [("root", false), ("typography", true)] {
        if next {
            ui.tap(&mut reader, 400, 750)?;
            ui.tap(&mut reader, 240, 150)?;
        }
        let pixels = ui.render(&mut reader, 4)?;
        let mut encoder =
            png::Encoder::new(File::create(output.join(format!("{name}.png")))?, 480, 800);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&pixels)?;
    }
    let mut reader = Reader::new(
        Document::open(root.join("tests/fixtures/supplemental-notes.epub"))?,
        Fonts::new(Font::load(root.join("tests/fixtures/DejaVuSerif.ttf"))?),
        ReaderState::default(),
    )?;
    let mut ui = ReaderUi::default();
    ui.open();
    for _ in 0..5 {
        ui.turn(&mut reader, true)?;
    }
    ui.select(&mut reader)?;
    ui.select(&mut reader)?;
    while ui.is_preview_opening() {
        ui.poll_preview(&mut reader);
    }
    let pixels = ui.render(&mut reader, 4)?;
    let mut encoder = png::Encoder::new(File::create(output.join("link-preview.png"))?, 480, 800);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&pixels)?;
    Ok(())
}
