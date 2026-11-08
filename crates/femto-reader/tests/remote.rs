use femto_reader::{
    controller::{Effect, Reader},
    document::Document,
    layout::{Font, Fonts},
    remote::{RemoteControl, Submit},
    state::ReaderState,
    ui::ReaderUi,
};

const MAP: &[u8] = &[
    0x05, 7, 0x19, 0, 0x29, 255, 0x15, 0, 0x26, 255, 0, 0x75, 8, 0x95, 6, 0x81, 0,
];
const NEXT: &[u8] = &[0x4e, 0, 0, 0, 0, 0];

#[test]
fn gatt_routes_are_installed_atomically_and_bound_to_the_session() {
    use femto_reader::remote::InputReport;
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    let mut remote = RemoteControl::default();
    let report = InputReport {
        value_handle: 10,
        cccd_handle: 11,
        reference: [0, 1],
    };
    let token = remote.connect_gatt(MAP, &[report]).unwrap();
    remote.poll(&mut reader, &mut ui).unwrap();
    assert!(remote.connect_gatt(MAP, &[]).is_err());
    let before = reader.position();
    assert_eq!(remote.submit_gatt(token, 10, NEXT), Submit::Queued);
    assert_eq!(remote.poll(&mut reader, &mut ui).unwrap(), Effect::Redraw);
    assert_ne!(reader.position(), before);

    let current = remote
        .connect_gatt(
            MAP,
            &[InputReport {
                value_handle: 20,
                cccd_handle: 21,
                ..report
            }],
        )
        .unwrap();
    assert_eq!(remote.submit_gatt(token, 10, NEXT), Submit::Stale);
    assert_eq!(remote.submit_gatt(token, 99, NEXT), Submit::Stale);
    remote.disconnect(token);
    assert!(remote.is_active());
    remote.poll(&mut reader, &mut ui).unwrap();
    let before = reader.position();
    assert_eq!(remote.submit_gatt(current, 20, NEXT), Submit::Queued);
    assert_eq!(remote.submit_gatt(current, 10, NEXT), Submit::Invalidated);
    assert!(!remote.is_active());
    remote.poll(&mut reader, &mut ui).unwrap();
    remote.poll(&mut reader, &mut ui).unwrap();
    assert_eq!(reader.position(), before);

    let raw = remote.connect(MAP).unwrap();
    assert_eq!(remote.submit_gatt(raw, 20, NEXT), Submit::Invalidated);
}

fn reader() -> Reader {
    reader_with_state(ReaderState::default())
}

fn reader_with_state(state: ReaderState) -> Reader {
    Reader::new(
        Document::from_text(
            "Remote",
            include_str!("../../../tests/fixtures/device-reader.txt"),
        )
        .unwrap(),
        Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
                .unwrap(),
        ),
        state,
    )
    .unwrap()
}

#[test]
fn saved_bindings_restore_through_host_and_device_state_and_drive_reader() {
    use femto_reader::{
        controller::Action,
        journal::StateJournal,
        remote::{RemoteAction, RemoteBinding, SavedBindings},
    };
    let mut reader = reader();
    let beginning = reader.position();
    reader.dispatch(Action::Next).unwrap();
    let next = reader.position();
    assert_ne!(next, beginning);
    reader.record_position(100);
    reader.state.remote_bindings = Some(
        SavedBindings::new(&[RemoteBinding {
            usage: 0x7004e,
            action: RemoteAction::Previous,
        }])
        .unwrap(),
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reader.json");
    reader.state.save(&path).unwrap();
    let restored = ReaderState::load(&path).unwrap();
    assert_eq!(restored.remote_bindings, reader.state.remote_bindings);
    let (mut journal, _) = StateJournal::open(dir.path()).unwrap();
    journal.save(&reader.state).unwrap();
    let device = StateJournal::open(dir.path()).unwrap().1;
    assert_eq!(device.remote_bindings, restored.remote_bindings);
    for state in [restored, device] {
        let mut reopened = reader_with_state(state);
        assert_eq!(reopened.position(), next);
        let mut remote = RemoteControl::default();
        let mut ui = ReaderUi::default();
        let token = remote.connect(MAP).unwrap();
        remote.poll(&mut reopened, &mut ui).unwrap();
        remote.submit(token, [0, 1], NEXT);
        assert_eq!(remote.poll(&mut reopened, &mut ui).unwrap(), Effect::Redraw);
        assert_eq!(reopened.position(), beginning);
    }
}

#[test]
fn absent_bindings_use_defaults_and_empty_bindings_disable_turns() {
    use femto_reader::remote::SavedBindings;
    let old = serde_json::to_value(ReaderState::default()).unwrap();
    assert!(old.get("remote_bindings").is_none());
    let restored: ReaderState = serde_json::from_value(old).unwrap();
    assert!(restored.remote_bindings.is_none());
    let mut reader = reader_with_state(restored);
    let mut remote = RemoteControl::default();
    let mut ui = ReaderUi::default();
    let token = remote.connect(MAP).unwrap();
    remote.poll(&mut reader, &mut ui).unwrap();
    let before = reader.position();
    reader.state.remote_bindings = Some(SavedBindings::new(&[]).unwrap());
    let serialized = serde_json::to_value(&reader.state).unwrap();
    assert_eq!(serialized["remote_bindings"], serde_json::json!([]));
    reader.state = serde_json::from_value(serialized).unwrap();
    remote.submit(token, [0, 1], NEXT);
    assert_eq!(
        remote.poll(&mut reader, &mut ui).unwrap(),
        Effect::Unchanged
    );
    assert_eq!(reader.position(), before);
    reader.state.remote_bindings = None;
    remote.submit(token, [0, 1], NEXT);
    assert_eq!(
        remote.poll(&mut reader, &mut ui).unwrap(),
        Effect::Unchanged
    );
    remote.submit(token, [0, 1], &[0; 6]);
    remote.poll(&mut reader, &mut ui).unwrap();
    remote.submit(token, [0, 1], NEXT);
    assert_eq!(remote.poll(&mut reader, &mut ui).unwrap(), Effect::Redraw);
    assert_ne!(reader.position(), before);
}

#[test]
fn invalid_persisted_remote_bindings_fail_loading() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reader.json");
    let valid: Vec<_> = (4..20)
        .map(|id| serde_json::json!({"usage": 0x70000 | id, "action": "next"}))
        .collect();
    let mut data = serde_json::to_value(ReaderState::default()).unwrap();
    data["remote_bindings"] = serde_json::json!(valid);
    std::fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
    assert_eq!(
        ReaderState::load(&path)
            .unwrap()
            .remote_bindings
            .unwrap()
            .entries()
            .len(),
        16
    );
    let mut too_many = valid.clone();
    too_many.push(serde_json::json!({"usage": 0x70020, "action": "next"}));
    for invalid in [
        serde_json::json!(too_many),
        serde_json::json!([valid[0], valid[0]]),
        serde_json::json!([{"usage": 0x70001, "action": "next"}]),
        serde_json::json!([{"usage": 0xff004e, "action": "next"}]),
        serde_json::json!([{"usage": 0x7004e, "action": "sideways"}]),
        serde_json::json!([{"usage": 0x7004e, "action": "next", "actoin": "previous"}]),
        serde_json::json!([{"usage": -1, "action": "next"}]),
    ] {
        data["remote_bindings"] = invalid;
        std::fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
        assert!(ReaderState::load(&path).is_err());
    }
}

#[test]
fn decoded_remote_press_turns_once_and_previous_restores_exact_page() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    let mut remote = RemoteControl::default();
    let start = reader.position();
    let pixels = reader.render(4).unwrap();
    let token = remote.connect(MAP).unwrap();
    remote.poll(&mut reader, &mut ui).unwrap();
    assert_eq!(remote.submit(token, [0, 1], NEXT), Submit::Queued);
    assert_eq!(remote.poll(&mut reader, &mut ui).unwrap(), Effect::Redraw);
    assert_ne!(reader.position(), start);
    let next = reader.position();
    remote.submit(token, [0, 1], NEXT);
    assert_eq!(
        remote.poll(&mut reader, &mut ui).unwrap(),
        Effect::Unchanged
    );
    assert_eq!(reader.position(), next);
    remote.submit(token, [0, 1], &[0; 6]);
    remote.poll(&mut reader, &mut ui).unwrap();
    remote.submit(token, [0, 1], &[0x4b, 0, 0, 0, 0, 0]);
    remote.poll(&mut reader, &mut ui).unwrap();
    assert_eq!(reader.position(), start);
    assert_eq!(reader.render(4).unwrap(), pixels);
}

#[test]
fn menus_consume_presses_without_changing_selection_or_replaying_after_close() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    let mut remote = RemoteControl::default();
    let token = remote.connect(MAP).unwrap();
    remote.poll(&mut reader, &mut ui).unwrap();
    ui.open();
    let menu = ui.render(&mut reader, 4).unwrap();
    let position = reader.position();
    remote.submit(token, [0, 1], NEXT);
    assert_eq!(
        remote.poll(&mut reader, &mut ui).unwrap(),
        Effect::Unchanged
    );
    assert_eq!(ui.render(&mut reader, 4).unwrap(), menu);
    assert_eq!(reader.position(), position);
    ui.back(&mut reader).unwrap();
    remote.submit(token, [0, 1], NEXT);
    assert_eq!(
        remote.poll(&mut reader, &mut ui).unwrap(),
        Effect::Unchanged
    );
    remote.submit(token, [0, 1], &[0; 6]);
    remote.poll(&mut reader, &mut ui).unwrap();
    remote.submit(token, [0, 1], NEXT);
    assert_eq!(remote.poll(&mut reader, &mut ui).unwrap(), Effect::Redraw);
    assert_ne!(reader.position(), position);
}

#[test]
fn disconnect_overflow_and_decode_error_prevent_queued_page_turns() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    let mut remote = RemoteControl::default();
    let position = reader.position();
    let token = remote.connect(MAP).unwrap();
    remote.submit(token, [0, 1], NEXT);
    remote.disconnect(token);
    remote.poll(&mut reader, &mut ui).unwrap();
    assert!(!remote.is_active());
    assert_eq!(reader.position(), position);
    let current = remote.connect(MAP).unwrap();
    assert_eq!(remote.submit(token, [0, 1], NEXT), Submit::Stale);
    for _ in 0..16 {
        assert_eq!(remote.submit(current, [0, 1], NEXT), Submit::Queued);
    }
    assert_eq!(remote.submit(current, [0, 1], NEXT), Submit::Invalidated);
    remote.poll(&mut reader, &mut ui).unwrap();
    assert!(!remote.is_active());
    assert_eq!(reader.position(), position);
    let current = remote.connect(MAP).unwrap();
    remote.poll(&mut reader, &mut ui).unwrap();
    remote.submit(current, [0, 1], &[1]);
    remote.submit(current, [0, 1], NEXT);
    assert!(remote.poll(&mut reader, &mut ui).is_err());
    assert!(!remote.is_active());
    remote.poll(&mut reader, &mut ui).unwrap();
    assert_eq!(
        remote.poll(&mut reader, &mut ui).unwrap(),
        Effect::Unchanged
    );
    assert_eq!(reader.position(), position);
}

#[test]
fn reflow_consumes_remote_press_without_moving_anchor() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    let mut remote = RemoteControl::default();
    let token = remote.connect(MAP).unwrap();
    remote.poll(&mut reader, &mut ui).unwrap();
    let position = reader.position();
    ui.resize(&reader, 2.0).unwrap();
    assert!(ui.is_reflowing());
    remote.submit(token, [0, 1], NEXT);
    assert_eq!(
        remote.poll(&mut reader, &mut ui).unwrap(),
        Effect::Unchanged
    );
    assert_eq!(reader.position(), position);
    while ui.is_reflowing() {
        ui.poll_reflow(&mut reader);
    }
    remote.submit(token, [0, 1], NEXT);
    assert_eq!(
        remote.poll(&mut reader, &mut ui).unwrap(),
        Effect::Unchanged
    );
    assert_eq!(reader.position(), position);
}

#[test]
fn remote_boundary_turn_uses_cancellable_cooperative_navigation() {
    use femto_reader::controller::Action;
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut reader = Reader::new(
        Document::open(root.join("sample.epub")).unwrap(),
        Fonts::new(Font::load(root.join("DejaVuSerif.ttf")).unwrap()),
        ReaderState::default(),
    )
    .unwrap();
    reader.dispatch(Action::Chapter(1)).unwrap();
    let before = reader.position();
    let pixels = reader.render(4).unwrap();
    let mut ui = ReaderUi::default();
    let mut remote = RemoteControl::default();
    let token = remote.connect(MAP).unwrap();
    remote.poll(&mut reader, &mut ui).unwrap();
    let previous = [0x4b, 0, 0, 0, 0, 0];
    remote.submit(token, [0, 1], &previous);
    remote.poll(&mut reader, &mut ui).unwrap();
    assert!(ui.is_navigating());
    assert_eq!(reader.position(), before);
    ui.back(&mut reader).unwrap();
    assert!(!ui.is_navigating());
    assert_eq!(reader.render(4).unwrap(), pixels);
    remote.submit(token, [0, 1], &[0; 6]);
    remote.poll(&mut reader, &mut ui).unwrap();
    remote.submit(token, [0, 1], &previous);
    remote.poll(&mut reader, &mut ui).unwrap();
    assert!(ui.is_navigating());
    remote.submit(token, [0, 1], NEXT);
    assert_eq!(
        remote.poll(&mut reader, &mut ui).unwrap(),
        Effect::Unchanged
    );
    for _ in 0..1000 {
        if !ui.is_navigating() {
            break;
        }
        ui.poll_navigation(&mut reader);
    }
    assert!(!ui.is_navigating());
    assert_eq!(reader.position().chapter, 0);
    assert_eq!(reader.page_index() + 1, reader.page_count());
    remote.submit(token, [0, 1], NEXT);
    assert_eq!(
        remote.poll(&mut reader, &mut ui).unwrap(),
        Effect::Unchanged
    );
    remote.submit(token, [0, 1], &[0; 6]);
    remote.poll(&mut reader, &mut ui).unwrap();
    remote.submit(token, [0, 1], NEXT);
    remote.poll(&mut reader, &mut ui).unwrap();
    for _ in 0..1000 {
        if !ui.is_navigating() {
            break;
        }
        ui.poll_navigation(&mut reader);
    }
    assert!(!ui.is_navigating());
    assert_eq!(reader.position(), before);
    assert_eq!(reader.render(4).unwrap(), pixels);
}

#[test]
fn invalid_replacement_descriptor_preserves_current_remote() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    let mut remote = RemoteControl::default();
    let token = remote.connect(MAP).unwrap();
    remote.poll(&mut reader, &mut ui).unwrap();
    assert!(remote.connect(&[0xff]).is_err());
    assert!(remote.is_active());
    assert_eq!(remote.submit(token, [0, 1], NEXT), Submit::Queued);
    assert_eq!(remote.poll(&mut reader, &mut ui).unwrap(), Effect::Redraw);
}
