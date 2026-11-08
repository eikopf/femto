use femto_reader::{
    controller::{Action, Reader},
    document::{Block, Document},
    layout::{Font, Fonts},
    navigation,
    state::ReaderState,
};
use std::path::PathBuf;
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}
fn reader() -> Reader {
    Reader::new(
        Document::open(fixture("navigation.epub")).unwrap(),
        Fonts::new(Font::load(fixture("DejaVuSerif.ttf")).unwrap()),
        ReaderState::default(),
    )
    .unwrap()
}

#[test]
fn metric_link_targets_cover_accents_and_mixed_sizes_without_crossing_lines() {
    let mut state = ReaderState::default();
    state.layout.line_height = 1.0;
    let reader = Reader::new(
        Document::open(fixture("metric-links.epub")).unwrap(),
        Fonts::new(Font::load(fixture("DejaVuSans.ttf")).unwrap()),
        state,
    )
    .unwrap();
    assert_eq!(reader.page_links().len(), 3);
    let lines = &reader.page().lines;
    assert_eq!(lines.len(), 3);
    for (index, line) in lines.iter().enumerate() {
        let glyph = &line.glyphs[0];
        let x = (glyph.x + glyph.advance / 2.0) as u32;
        let top = (line.baseline - line.ascent).ceil() as u32;
        let bottom = (line.baseline + line.descent).ceil() as u32;
        assert_eq!(reader.link_at(x, top), Some(index));
        assert_eq!(reader.link_at(x, bottom - 1), Some(index));
        assert_eq!(reader.link_at(x, top - 1), None);
        assert_eq!(reader.link_at(x, bottom), None);
        if index < 2 {
            assert!(
                (top as f32) < glyph.y - glyph.size,
                "fixture {index} must exercise the area missed by font-size targets: top={top}, glyph={glyph:?}, line={line:?}"
            );
        }
    }
}

#[test]
fn taps_follow_shaped_links_after_reflow_and_preserve_back_history() {
    use femto_reader::controller::Effect;
    let mut reader = reader();
    for size in [18.0, 30.0, 42.0] {
        let mut config = reader.state.layout;
        config.font_size = size;
        reader.dispatch(Action::Reflow(config)).unwrap();
        let origin = reader.position();
        let link = reader.page_links()[0];
        let tap = reader
            .page()
            .lines
            .iter()
            .flat_map(|line| {
                line.glyphs.iter().filter_map(|glyph| {
                    let location = femto_reader::layout::Locator {
                        byte: glyph.cluster,
                        ..line.start
                    };
                    (location >= link.start && location < link.end && glyph.advance > 2.0)
                        .then_some((
                            (glyph.x + glyph.advance * 0.5) as u32,
                            (glyph.y - glyph.size * 0.5) as u32,
                        ))
                })
            })
            .next()
            .expect("visible linked glyph");
        assert_eq!(reader.link_at(tap.0, tap.1), Some(0));
        assert_eq!(reader.link_at(0, 0), None);
        assert_eq!(reader.link_at(config.width, config.height), None);
        assert_eq!(
            reader.dispatch(Action::Tap { x: tap.0, y: tap.1 }).unwrap(),
            Effect::Redraw
        );
        assert_eq!(reader.position().chapter, 1);
        reader.dispatch(Action::Back).unwrap();
        assert_eq!(reader.position(), origin);
        assert_eq!(
            reader
                .dispatch(Action::Tap {
                    x: config.width / 2,
                    y: 0
                })
                .unwrap(),
            Effect::Menu
        );
    }
}

#[test]
fn unsupported_link_taps_preserve_the_page_instead_of_turning_it() {
    let mut reader = reader();
    for index in [1, 2] {
        let link = reader.page_links()[index];
        let glyph = reader
            .page()
            .lines
            .iter()
            .find_map(|line| {
                line.glyphs.iter().find(|glyph| {
                    let position = femto_reader::layout::Locator {
                        byte: glyph.cluster,
                        ..line.start
                    };
                    position >= link.start && position < link.end && glyph.advance > 2.0
                })
            })
            .unwrap();
        let (x, y) = (
            (glyph.x + glyph.advance * 0.5) as u32,
            (glyph.y - glyph.size * 0.5) as u32,
        );
        let origin = reader.position();
        assert_eq!(reader.link_at(x, y), Some(index));
        assert!(reader.dispatch(Action::Tap { x, y }).is_err());
        assert_eq!(reader.position(), origin);
    }
}

#[test]
fn tap_targets_follow_bidi_clusters_ligatures_and_image_placement() {
    let mut reader = Reader::new(
        Document::open(fixture("tap-links.epub")).unwrap(),
        Fonts::new(Font::load(fixture("DejaVuSans.ttf")).unwrap()),
        ReaderState::default(),
    )
    .unwrap();
    assert_eq!(reader.page_links().len(), 3);
    let marked = reader.render(2).unwrap();
    assert!(
        reader.page().lines[0]
            .glyphs
            .windows(2)
            .any(|pair| pair[0].cluster > pair[1].cluster)
    );
    for (index, link) in reader.page_links().iter().take(2).enumerate() {
        let mut checked = 0;
        for line in &reader.page().lines {
            for glyph in &line.glyphs {
                let location = femto_reader::layout::Locator {
                    byte: glyph.cluster,
                    ..line.start
                };
                if location >= link.start && location < link.end && glyph.advance > 2.0 {
                    let x = (glyph.x + glyph.advance * 0.5) as usize;
                    let y = (glyph.y + glyph.size * 0.12).round() as usize;
                    assert_eq!(marked[y * reader.state.layout.width as usize + x], 0);
                    assert_eq!(
                        reader.link_at(
                            (glyph.x + glyph.advance * 0.5) as u32,
                            (glyph.y - glyph.size * 0.5) as u32
                        ),
                        Some(index)
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 3);
    }
    let image = &reader.page().images[0];
    assert_eq!(
        reader.link_at(
            image.x + image.size.width / 2,
            image.y + image.size.height / 2
        ),
        Some(2)
    );
    assert_eq!(reader.link_at(image.x + image.size.width, image.y), None);
}

#[test]
fn link_underlines_add_black_ink_without_reflow_or_changing_plain_text() {
    use femto_reader::layout::rasterize_display;
    let mut reader = reader();
    let fonts = Fonts::new(Font::load(fixture("DejaVuSerif.ttf")).unwrap());
    let position = reader.position();
    let page_end = reader.page().end;
    let count = reader.page_count();
    for levels in [2, 4, 256] {
        let original = rasterize_display(
            reader.page(),
            &fonts,
            reader.state.layout,
            reader.state.text_raster,
            levels,
            |_| panic!("text-only page"),
        )
        .unwrap();
        let marked = reader.render(levels).unwrap();
        let changed: Vec<_> = original
            .iter()
            .zip(&marked)
            .enumerate()
            .filter(|(_, (before, after))| before != after)
            .collect();
        assert!(changed.len() > 30);
        for (offset, (_, after)) in changed {
            assert_eq!(*after, 0);
            let x = (offset % reader.state.layout.width as usize) as u32;
            let y = (offset / reader.state.layout.width as usize) as u32;
            // The underline stays inside the linked text's existing tap area.
            assert!(reader.link_at(x, y).is_some() || reader.link_at(x + 1, y).is_some());
        }
        assert_eq!(reader.position(), position);
        assert_eq!(reader.page().end, page_end);
        assert_eq!(reader.page_count(), count);
    }
    let fonts = || Fonts::new(Font::load(fixture("DejaVuSerif.ttf")).unwrap());
    let mut plain = Reader::new(
        Document::from_text("Plain", "Text without links").unwrap(),
        fonts(),
        ReaderState::default(),
    )
    .unwrap();
    let original = rasterize_display(
        plain.page(),
        &fonts(),
        plain.state.layout,
        plain.state.text_raster,
        2,
        |_| panic!("text-only page"),
    )
    .unwrap();
    assert_eq!(plain.render(2).unwrap(), original);
}

#[test]
fn archive_relative_paths_and_fragments_are_resolved_without_external_access() {
    assert_eq!(
        navigation::resolve("nav/toc.xhtml", "../text/one.xhtml#caf%C3%A9").unwrap(),
        ("text/one.xhtml".into(), Some("café".into()))
    );
    assert_eq!(
        navigation::resolve("text/one.xhtml", "#x").unwrap(),
        ("text/one.xhtml".into(), Some("x".into()))
    );
    for uri in [
        "https://example.org/a",
        "//example.org/a",
        "/etc/passwd",
        "../../outside",
        "%2e%2e/%2e%2e/out",
        "file%3Asecret",
        "foo?x=1",
        "bad%",
        "bad%ff",
        "foo\\bar",
    ] {
        assert!(navigation::resolve("text/one.xhtml", uri).is_err(), "{uri}");
    }
}

#[test]
fn ids_and_links_use_the_rendered_text_offsets() {
    let mut document = Document::open(fixture("navigation.epub")).unwrap();
    let chapter = document.chapter(0).unwrap();
    assert_eq!(chapter.links.len(), 3);
    assert_eq!(chapter.links[0].label, "the first footnote");
    let link = &chapter.links[0];
    let Block::Text(text) = &chapter.blocks[link.start.block] else {
        panic!()
    };
    assert_eq!(&text.text[link.start.byte..link.end.byte], link.label);
    let inline = document
        .resolve_link("text/one.xhtml", "two.xhtml#inline%20anchor")
        .unwrap();
    let chapter = document.chapter(inline.chapter).unwrap();
    let Block::Text(text) = &chapter.blocks[inline.block] else {
        panic!()
    };
    assert!(text.text[inline.byte..].starts_with("the inline target"));
    assert!(inline.byte > 0);
    assert!(
        document
            .resolve_link("text/one.xhtml", "two.xhtml#A%26B")
            .is_ok()
    );
}

#[test]
fn footnotes_contents_reflow_and_back_navigation_share_source_locations() {
    let mut reader = reader();
    let origin = reader.page().start;
    assert_eq!(reader.page_links().len(), 3);
    for index in [1, 2] {
        assert!(reader.dispatch(Action::Link(index)).is_err());
        assert_eq!(reader.page().start, origin);
    }
    reader.dispatch(Action::Link(0)).unwrap();
    assert_eq!(reader.page().start.chapter, 1);
    assert!(reader.page_index() > 0);
    assert!(
        reader
            .page()
            .lines
            .iter()
            .any(|line| line.text.contains("footnote target"))
    );
    let target = reader.page().start;
    reader.dispatch(Action::Back).unwrap();
    assert_eq!(reader.page().start, origin);
    reader.dispatch(Action::Contents(1)).unwrap();
    assert_eq!(reader.page().start, target);
    let mut config = reader.state.layout;
    config.font_size = 30.0;
    reader.dispatch(Action::Reflow(config)).unwrap();
    reader.dispatch(Action::Contents(1)).unwrap();
    assert!(
        reader
            .page()
            .lines
            .iter()
            .any(|line| line.text.contains("footnote target"))
    );
    // Follow the explicit return reference, independently of back history.
    assert_eq!(reader.page_links().len(), 1);
    reader.dispatch(Action::Link(0)).unwrap();
    assert_eq!(reader.page().start.chapter, 0);
}

#[test]
fn supplemental_notes_resume_reflow_bookmark_and_return_without_joining_the_spine() {
    use femto_reader::controller::Effect;
    let fonts = || Fonts::new(Font::load(fixture("DejaVuSerif.ttf")).unwrap());
    let document = || Document::open(fixture("supplemental-notes.epub")).unwrap();
    let mut reader = Reader::new(document(), fonts(), ReaderState::default()).unwrap();
    assert_eq!(reader.document.metadata.chapters.len(), 1);
    assert_eq!(reader.document.document_count(), 3); // spine, navigation document, note
    let origin = reader.position();
    let original_progress = reader.book_progress_percent();
    assert!(original_progress.is_some());
    reader.dispatch(Action::Link(0)).unwrap();
    let note = reader.position();
    assert_eq!(note.chapter, 2);
    assert_eq!(reader.book_progress_percent(), None);
    assert!(
        reader
            .page()
            .lines
            .iter()
            .any(|l| l.text.contains("supplemental footnote"))
    );
    assert_eq!(
        reader.dispatch(Action::Previous).unwrap(),
        Effect::Unchanged
    );
    assert_eq!(reader.dispatch(Action::Next).unwrap(), Effect::Unchanged);
    reader.dispatch(Action::ToggleBookmark).unwrap();
    let mut config = reader.state.layout;
    config.font_size = 28.0;
    reader.dispatch(Action::Reflow(config)).unwrap();
    assert_eq!(reader.position(), note);
    reader.record_position(1);
    let saved = reader.state.clone();
    reader.dispatch(Action::Back).unwrap();
    assert_eq!(reader.position(), origin);
    assert!(reader.book_progress_percent().is_some());
    reader.dispatch(Action::Bookmark(0)).unwrap();
    assert_eq!(reader.position(), note);
    reader.dispatch(Action::Link(0)).unwrap();
    assert_eq!(reader.position().chapter, 0);
    let mut reopened = Reader::new(document(), fonts(), saved).unwrap();
    assert_eq!(reopened.position(), note);
    assert_eq!(reopened.book_progress_percent(), None);
    reopened.render(2).unwrap();
    reopened.dispatch(Action::Link(0)).unwrap();
    assert_eq!(reopened.position().chapter, 0);
    for _ in 0..reopened.page_count() + 1 {
        reopened.dispatch(Action::Next).unwrap();
    }
    assert_eq!(reopened.position().chapter, 0);
    assert_eq!(reopened.dispatch(Action::Next).unwrap(), Effect::Unchanged);
}

#[test]
fn link_preview_pages_and_renders_without_moving_or_saving_the_reader() {
    use femto_reader::controller::Effect;
    let mut reader = reader();
    reader.record_position(10);
    let origin = reader.position();
    let state = serde_json::to_vec(&reader.state).unwrap();
    let original_pixels = reader.render(4).unwrap();
    let mut config = reader.state.layout;
    config.height -= 160;
    let mut preview = reader.preview_link(0, config).unwrap();
    assert!(
        preview
            .page()
            .lines
            .iter()
            .any(|l| l.text.contains("footnote target"))
    );
    assert_eq!(preview.page().start.chapter, 1);
    assert!(preview.page_index() > 0);
    let pixels = reader.render_preview(&preview, 4).unwrap();
    assert_eq!(pixels.len(), (config.width * config.height) as usize);
    assert!(pixels.iter().all(|p| [0, 85, 170, 255].contains(p)));
    let initial_index = preview.page_index();
    assert_eq!(
        reader.turn_preview(&mut preview, false).unwrap(),
        Effect::Redraw
    );
    assert_eq!(preview.page_index(), initial_index - 1);
    assert_eq!(reader.position(), origin);
    assert_eq!(serde_json::to_vec(&reader.state).unwrap(), state);
    assert_eq!(reader.render(4).unwrap(), original_pixels);
    assert_eq!(reader.dispatch(Action::Back).unwrap(), Effect::Unchanged);
    let destination = preview.page().start;
    reader.follow_preview(&preview).unwrap();
    assert_eq!(reader.position(), destination);
    reader.dispatch(Action::Back).unwrap();
    assert_eq!(reader.position(), origin);
}

#[test]
fn failed_and_stale_previews_preserve_reading_state() {
    let mut reader = reader();
    let origin = reader.position();
    for index in [1, 2, usize::MAX] {
        assert!(reader.preview_link(index, reader.state.layout).is_err());
        assert_eq!(reader.position(), origin);
    }
    let mut preview = reader.preview_link(0, reader.state.layout).unwrap();
    let mut config = reader.state.layout;
    config.font_size += 2.0;
    reader.dispatch(Action::Reflow(config)).unwrap();
    let after_reflow = reader.position();
    assert!(reader.render_preview(&preview, 4).is_err());
    assert!(reader.turn_preview(&mut preview, true).is_err());
    assert!(reader.follow_preview(&preview).is_err());
    assert_eq!(reader.position(), after_reflow);
}

#[test]
fn supplemental_note_preview_can_be_dismissed_or_followed() {
    use femto_reader::controller::Effect;
    let mut reader = Reader::new(
        Document::open(fixture("supplemental-notes.epub")).unwrap(),
        Fonts::new(Font::load(fixture("DejaVuSerif.ttf")).unwrap()),
        ReaderState::default(),
    )
    .unwrap();
    let origin = reader.position();
    let mut preview = reader.preview_link(0, reader.state.layout).unwrap();
    assert!(
        preview
            .page()
            .lines
            .iter()
            .any(|l| l.text.contains("supplemental footnote"))
    );
    assert_eq!(reader.position(), origin);
    while reader.turn_preview(&mut preview, true).unwrap() != Effect::Unchanged {}
    assert_eq!(preview.page_index() + 1, preview.page_count());
    reader.follow_preview(&preview).unwrap();
    assert!(reader.position().chapter >= reader.document.metadata.chapters.len());
    reader.dispatch(Action::Back).unwrap();
    assert_eq!(reader.position(), origin);
    let preview = reader.preview_link(0, reader.state.layout).unwrap();
    drop(preview);
    assert_eq!(reader.dispatch(Action::Back).unwrap(), Effect::Unchanged);
}

#[test]
fn note_reference_markers_survive_inline_styles_and_do_not_leak_to_adjacent_links() {
    let mut document = Document::open(fixture("noterefs.epub")).unwrap();
    let chapter = document.chapter(0).unwrap();
    assert_eq!(
        chapter.links.iter().map(|l| l.noteref).collect::<Vec<_>>(),
        [true, true, false, false, false, true, true, true]
    );
    assert_eq!(chapter.links[0].label, "EPUB reference");
    assert_eq!(chapter.links[1].label, "ARIA reference");
    assert_eq!(chapter.links[2].label, "Ordinary link");
    assert_eq!(chapter.links[7].label, "Image reference");
}

#[test]
fn note_references_resolve_namespace_aliases_and_scoped_rebindings() {
    let mut document = Document::open(fixture("note-namespaces.epub")).unwrap();
    let chapter = document.chapter(0).unwrap();
    assert_eq!(
        chapter.links.iter().map(|l| l.noteref).collect::<Vec<_>>(),
        [true, false, false, false, true, true, false, true]
    );
    assert_eq!(chapter.links[0].label, "Inherited alias");
    assert_eq!(chapter.links[4].label, "Local declaration");
    assert_eq!(chapter.links[7].label, "After empty element");
}

#[test]
fn preview_indexing_is_cooperative_and_rejects_incomplete_or_stale_jobs() {
    let mut reader = reader();
    let origin = reader.position();
    let config = reader.state.layout;
    let mut job = reader.begin_preview(0, config).unwrap();
    assert_eq!(job.indexed_pages(), 0);
    assert!(!reader.step_preview(&mut job).unwrap());
    assert_eq!(job.indexed_pages(), 1);
    assert_eq!(reader.position(), origin);
    assert!(reader.finish_preview(job).is_err());
    let mut job = reader.begin_preview(0, config).unwrap();
    while !reader.step_preview(&mut job).unwrap() {}
    let preview = reader.finish_preview(job).unwrap();
    let expected = reader.preview_link(0, config).unwrap();
    assert_eq!(
        reader.render_preview(&preview, 4).unwrap(),
        reader.render_preview(&expected, 4).unwrap()
    );
    let mut job = reader.begin_preview(0, config).unwrap();
    reader.dispatch(Action::Next).unwrap();
    let position = reader.position();
    assert!(reader.step_preview(&mut job).is_err());
    assert!(reader.finish_preview(job).is_err());
    assert_eq!(reader.position(), position);
}

#[test]
fn cooperative_link_and_preview_follow_preserve_source_and_back_history() {
    use femto_reader::controller::Effect;
    let mut reader = Reader::new(
        Document::open(fixture("supplemental-notes.epub")).unwrap(),
        Fonts::new(Font::load(fixture("DejaVuSerif.ttf")).unwrap()),
        ReaderState::default(),
    )
    .unwrap();
    let origin = reader.position();
    let original = reader.render(4).unwrap();
    let mut job = reader.begin_navigation(Action::Link(0)).unwrap().unwrap();
    let incomplete = reader.begin_navigation(Action::Link(0)).unwrap().unwrap();
    assert!(reader.finish_navigation(incomplete).is_err());
    while !reader.step_navigation(&mut job).unwrap() {}
    reader.finish_navigation(job).unwrap();
    let direct_position = reader.position();
    let direct = reader.render(4).unwrap();
    reader.dispatch(Action::Back).unwrap();
    assert_eq!(reader.position(), origin);
    assert_eq!(reader.render(4).unwrap(), original);
    let mut config = reader.state.layout;
    config.height = 300;
    let mut preview = reader.preview_link(0, config).unwrap();
    let job = reader.begin_follow_preview(&preview).unwrap().unwrap();
    drop(job);
    assert_eq!(reader.dispatch(Action::Back).unwrap(), Effect::Unchanged);
    let mut job = reader.begin_follow_preview(&preview).unwrap().unwrap();
    while !reader.step_navigation(&mut job).unwrap() {}
    reader.finish_navigation(job).unwrap();
    assert_eq!(reader.position(), direct_position);
    assert_eq!(reader.render(4).unwrap(), direct);
    assert!(reader.begin_follow_preview(&preview).is_err());
    reader.dispatch(Action::Back).unwrap();
    assert_eq!(reader.position(), origin);
    reader.turn_preview(&mut preview, true).unwrap();
    let target = preview.page().start;
    let mut job = reader.begin_follow_preview(&preview).unwrap().unwrap();
    while !reader.step_navigation(&mut job).unwrap() {}
    reader.finish_navigation(job).unwrap();
    assert_eq!(reader.position(), target);
    reader.dispatch(Action::Back).unwrap();
    assert_eq!(reader.position(), origin);
}
