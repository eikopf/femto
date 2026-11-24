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
/// must still show its entries with one of them selected. Instead it shows
/// the heading over an empty page, and the buttons appear dead until the
/// selection has been walked back into range.
#[test]
#[ignore = "bug: tailnet status rows that shrink leave the selection past the end and the panel blank"]
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
#[ignore = "ambiguous: catalog rows replaced by fewer leave the selection past the end"]
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
/// same, so the frame the frontend keeps on screen is stale.
#[test]
#[ignore = "bug: a keyboard tap that edits nothing moves the highlight but reports no change"]
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

/// The sync session asks "Keep which position?" over the reading page,
/// whenever `!ui.is_open()` (femto-kosync `session.rs`), and then waits for
/// `TakeRemote` or `KeepLocal`; until one arrives it starts no automatic
/// sync. A page turn across a chapter boundary is indexed over several polls
/// with the page still showing, so the question can arrive meanwhile. When
/// the turn then finishes, the panel is replaced by the one the turn started
/// from, and the question is gone without an answer.
#[test]
#[ignore = "bug: a sync question asked while a page turn is pending vanishes unanswered when the turn finishes"]
fn sync_question_survives_a_pending_page_turn() {
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
    ui.ask_sync_conflict(40, Some(10));
    assert!(ui.is_open());
    for _ in 0..16_384 {
        if !ui.is_navigating() {
            break;
        }
        ui.poll_navigation(&mut reader);
    }
    assert!(!ui.is_navigating());
    assert!(
        ui.is_open() || ui.take_kosync_event().is_some(),
        "the question was neither left on screen nor answered"
    );
}
