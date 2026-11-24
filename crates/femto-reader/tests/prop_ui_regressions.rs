//! Deterministic reductions of failures found by the interaction state
//! machine harness in `src/ui_prop.rs`. Each is ignored while its bug stands.
use femto_reader::{
    controller::{Effect, Reader},
    document::Document,
    layout::{Font, Fonts},
    state::ReaderState,
    ui::ReaderUi,
};

fn reader() -> Reader {
    Reader::new(
        Document::from_text(
            "Menus",
            include_str!("../../../tests/fixtures/device-reader.txt"),
        )
        .unwrap(),
        Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
                .unwrap(),
        ),
        ReaderState::default(),
    )
    .unwrap()
}

fn rows(count: usize) -> Vec<String> {
    (0..count).map(|index| format!("peer {index}")).collect()
}

/// The Tailnet panel with `status` rows and the selection moved down `down`
/// rows from the top.
fn tailnet(reader: &mut Reader, status: usize, down: usize) -> Vec<u8> {
    let mut ui = ReaderUi::default();
    ui.open_tailnet();
    ui.set_tailnet_status(&rows(status));
    for _ in 0..down {
        ui.turn(reader, true).unwrap();
    }
    ui.render(reader, 2).unwrap()
}

/// The tailnet status rows are replaced whenever the connection changes.
/// When they become fewer while the selection is on a later page, the panel
/// must still show its entries with one of them selected. It showed the
/// heading over an empty page, and the buttons appeared dead until the
/// selection had been walked back into range.
#[test]
fn tailnet_rows_that_shrink_keep_a_row_selected() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.open_tailnet();
    // Four actions and ten status rows fill three pages of six.
    ui.set_tailnet_status(&rows(10));
    while ui.swipe(&mut reader, true).unwrap() == Effect::Redraw {}
    ui.set_tailnet_status(&[]);
    let shown = ui.render(&mut reader, 2).unwrap();
    let valid: Vec<Vec<u8>> = (0..4).map(|down| tailnet(&mut reader, 0, down)).collect();
    assert!(
        valid.contains(&shown),
        "no entry of the shortened panel is selected"
    );
}

/// The same for the catalog list, which `set_catalog_rows` replaces in
/// place. The firmware only lets that list grow, so this is a weaker
/// contract: the documented use is "a list that grows while it is read".
#[test]
fn catalog_rows_that_shrink_keep_a_row_selected() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.set_catalog_view("Catalog", &rows(14)).unwrap();
    while ui.swipe(&mut reader, true).unwrap() == Effect::Redraw {}
    assert_eq!(ui.catalog_position(), Some(12));
    ui.set_catalog_rows(&rows(3)).unwrap();
    assert!(
        ui.catalog_position().unwrap() < 3,
        "selection {:?} of 3 rows",
        ui.catalog_position()
    );
}

/// An input that reports `Effect::Unchanged` tells the frontend not to draw.
/// Tapping a key that has nothing to do (Delete on an empty field, or a
/// letter when the field is full) moves the highlight to that key all the
/// same, so it must ask for the frame to be drawn.
#[test]
fn keyboard_tap_that_moves_the_highlight_asks_for_a_redraw() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.open();
    ui.choose(&mut reader, "Library").unwrap();
    ui.choose(&mut reader, "Search").unwrap();
    assert!(ui.is_editing() && ui.query().is_empty());
    let before = ui.render(&mut reader, 2).unwrap();
    // Delete is the third of the four keys in the bottom row.
    let effect = ui.tap(&mut reader, 300, 790).unwrap();
    let after = ui.render(&mut reader, 2).unwrap();
    assert!(
        effect == Effect::Redraw || before == after,
        "the view changed but the tap reported {effect:?}"
    );
}

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

/// The sync session asks "Keep which position?" over the reading page. A
/// page turn across a chapter boundary is indexed over several polls with
/// the page still showing, so the question can arrive meanwhile. When the
/// turn finishes, the panel is replaced by the one the turn started from,
/// which took the question away without an answer; the interface now
/// refuses the question until the turn is done.
#[test]
fn sync_question_is_refused_during_a_pending_page_turn() {
    let mut reader = Reader::new(
        Document::open(fixture("navigation.epub")).unwrap(),
        Fonts::new(Font::load(fixture("DejaVuSerif.ttf")).unwrap()),
        ReaderState::default(),
    )
    .unwrap();
    let mut ui = ReaderUi::default();
    // Forward to the turn that leaves the first chapter.
    for _ in 0..64 {
        ui.turn(&mut reader, true).unwrap();
        if ui.is_navigating() {
            break;
        }
    }
    assert!(ui.is_navigating() && !ui.is_open());
    assert!(!ui.ask_sync_conflict(40, Some(10)) && !ui.is_open());
    for _ in 0..16_384 {
        if !ui.is_navigating() {
            break;
        }
        ui.poll_navigation(&mut reader);
    }
    assert!(!ui.is_navigating());
    // Asked again once the turn is done, the question stays.
    assert!(ui.ask_sync_conflict(40, Some(10)) && ui.is_open());
}

/// Selecting an entry clears the error the menu is showing. When the entry
/// starts pending work, here the preview of a link, the input reports
/// `Effect::Unchanged`, which tells the frontend that the frame it has drawn
/// still stands (`ReaderUi::is_busy`: "The view stays as it was when the
/// work began"). The frame still shows the error.
#[test]
#[ignore = "bug: selecting an entry that starts pending work clears the error shown and reports no change"]
fn starting_a_link_preview_over_an_error_asks_for_a_redraw() {
    let mut reader = Reader::new(
        Document::open(fixture("navigation.epub")).unwrap(),
        Fonts::new(Font::load(fixture("DejaVuSerif.ttf")).unwrap()),
        ReaderState::default(),
    )
    .unwrap();
    let mut ui = ReaderUi::default();
    ui.open();
    ui.choose(&mut reader, "This book").unwrap();
    ui.choose(&mut reader, "Links on this page").unwrap();
    // The second link of the first page leaves the book and has no preview.
    assert_eq!(ui.turn(&mut reader, true).unwrap(), Effect::Redraw);
    assert_eq!(ui.select(&mut reader).unwrap(), Effect::Redraw);
    assert!(ui.error_message().is_some() && !ui.is_busy());
    // The first has one.
    assert_eq!(ui.turn(&mut reader, false).unwrap(), Effect::Redraw);
    let shown = ui.error_message().map(str::to_owned);
    assert!(shown.is_some(), "moving the selection keeps the error");
    let effect = ui.select(&mut reader).unwrap();
    assert!(ui.is_preview_opening());
    assert!(
        effect == Effect::Redraw || ui.error_message().map(str::to_owned) == shown,
        "the error went from the view but selecting the link reported {effect:?}"
    );
}
