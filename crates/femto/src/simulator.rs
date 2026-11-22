#[cfg(all(unix, not(target_os = "espidf")))]
mod host_trust;
#[cfg(all(unix, not(target_os = "espidf")))]
mod state_file;

#[cfg(unix)]
#[path = "../../../firmware/picco/src/catalog.rs"]
mod catalog_menu;
#[cfg(unix)]
mod host_http;
#[cfg(unix)]
mod simulator_tailnet;
#[cfg(unix)]
mod network {
    pub struct Transport;
    impl femto_catalog::http::Transport for Transport {
        fn connect(
            &mut self,
            url: &url::Url,
            deadline: std::time::Instant,
            cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
        ) -> std::io::Result<Box<dyn femto_catalog::http::Stream>> {
            femto_catalog::http::Transport::connect(
                &mut super::host_http::HostTransport { endpoint: None },
                url,
                deadline,
                cancelled,
            )
        }
    }
}

use anyhow::Result;
use clap::Parser;
use femto_reader::{
    controller::{Action, Effect, Reader, ReaderStartup},
    layout::{Font, Fonts},
    state::{Library, ReaderState},
    ui::ReaderUi,
};
use minifb::{InputCallback, Key, MouseButton, MouseMode, Window, WindowOptions};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq)]
enum InputEvent {
    Key(Key),
    Text(char),
}

#[derive(Default)]
struct KeyEvents {
    pressed: VecDeque<InputEvent>,
    held: Vec<Key>,
}

impl KeyEvents {
    fn push(&mut self, event: InputEvent) {
        if self.pressed.len() == 64 {
            self.pressed.pop_front();
        }
        self.pressed.push_back(event);
    }
}
struct Keyboard(Rc<RefCell<KeyEvents>>);
impl InputCallback for Keyboard {
    fn add_char(&mut self, value: u32) {
        if let Some(character) = char::from_u32(value) {
            self.0.borrow_mut().push(InputEvent::Text(character));
        }
    }
    fn set_key_state(&mut self, key: Key, down: bool) {
        let mut events = self.0.borrow_mut();
        if down && !events.held.contains(&key) {
            events.held.push(key);
            events.push(InputEvent::Key(key));
        } else if !down {
            events.held.retain(|held| *held != key);
        }
    }
}
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Parser)]
#[command(about = "Femto native e-paper reader simulator")]
struct Args {
    book: PathBuf,
    /// Offline StarDict .ifo file for selection lookup.
    #[arg(long)]
    dictionary: Option<PathBuf>,
    /// Directory of StarDict .ifo files for the dictionary chooser.
    #[arg(long, conflicts_with = "dictionary")]
    dictionary_directory: Option<PathBuf>,
    /// Directory containing catalog.url and optional catalog-auth.json; downloads go here.
    #[cfg(unix)]
    #[arg(long)]
    catalog_directory: Option<PathBuf>,
    /// Private KOReader account and server configuration.
    #[cfg(unix)]
    #[arg(long)]
    kosync_config: Option<PathBuf>,
    /// Directory containing private tailnet.json and the host identity subdirectory.
    #[cfg(unix)]
    #[arg(long, requires = "tailnet_endpoint")]
    tailnet_directory: Option<PathBuf>,
    /// Numeric coordinator address; TLS verifies the host in tailnet.json.
    #[cfg(unix)]
    #[arg(long, requires = "tailnet_directory")]
    tailnet_endpoint: Option<std::net::SocketAddr>,
    /// Explicit DER CA for the coordinator only; replaces Mozilla roots for this connection.
    #[cfg(unix)]
    #[arg(long, requires = "tailnet_directory")]
    tailnet_ca: Option<PathBuf>,
    /// Explicit DER CA for authorized relays, separate from coordinator and service trust.
    #[cfg(unix)]
    #[arg(long, requires = "tailnet_directory")]
    tailnet_relay_ca: Option<PathBuf>,
    /// Use DERP only; disable direct UDP, discovery probes and STUN.
    #[cfg(unix)]
    #[arg(long, requires = "tailnet_directory")]
    tailnet_relay_only: bool,
    /// Explicit DER CA for tailnet catalog/sync HTTPS; does not change LAN or control trust.
    #[cfg(unix)]
    #[arg(long, requires = "tailnet_directory")]
    tailnet_service_ca: Option<PathBuf>,
    /// Directory to scan for the shared library chooser.
    #[arg(long)]
    library: Option<PathBuf>,
    #[arg(long)]
    font: PathBuf,
    #[arg(long)]
    fallback_font: Vec<PathBuf>,
    #[arg(long, default_value = "output/simulator.json")]
    state: PathBuf,
    #[arg(long, default_value_t = 4, value_parser = levels)]
    levels: u16,
}
fn levels(value: &str) -> Result<u16, String> {
    match value.parse() {
        Ok(value @ (2 | 4 | 256)) => Ok(value),
        _ => Err("levels must be 2, 4, or 256".into()),
    }
}

fn save(reader: &mut Reader, path: &PathBuf) -> Result<()> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let access = reader
        .state
        .books
        .get(&reader.document.metadata.id)
        .filter(|book| book.position == reader.position())
        .map_or(now, |book| book.last_opened);
    reader.record_position(access);
    reader.state.save(path)?;
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    #[cfg(all(unix, not(target_os = "espidf")))]
    let state_file = state_file::StateFile::acquire(&args.state)?;
    #[cfg(all(unix, not(target_os = "espidf")))]
    let args = Args {
        state: state_file.path().to_owned(),
        ..args
    };
    let mut fonts = Fonts::new(Font::load(args.font)?);
    fonts.fallback = args
        .fallback_font
        .into_iter()
        .map(Font::load)
        .collect::<femto_reader::Result<_>>()?;
    let state = ReaderState::load(&args.state)?;
    state.layout.validate()?;
    let width = state.layout.width as usize;
    let height = state.layout.height as usize;
    let mut window = Window::new(
        "Femto - Opening book",
        width,
        height,
        WindowOptions::default(),
    )?;
    let keyboard = Rc::new(RefCell::new(KeyEvents::default()));
    window.set_input_callback(Box::new(Keyboard(keyboard.clone())));
    window.set_target_fps(30);
    let loading = vec![0x00ff_ffff; width * height];
    window.update_with_buffer(&loading, width, height)?;
    #[cfg(unix)]
    let library_directory = args.library.or_else(|| args.catalog_directory.clone());
    #[cfg(not(unix))]
    let library_directory = args.library;
    #[cfg(unix)]
    if let (Some(directory), Some(library)) = (&args.catalog_directory, &library_directory) {
        anyhow::ensure!(
            directory
                .canonicalize()?
                .starts_with(library.canonicalize()?),
            "catalog directory must be within the library directory"
        );
    }
    let cached_root = library_directory.as_ref().filter(|directory| {
        args.book
            .canonicalize()
            .ok()
            .zip(directory.canonicalize().ok())
            .is_some_and(|(book, root)| book.starts_with(root))
    });
    let mut startup = if let Some(root) = cached_root {
        ReaderStartup::from_cached_file(args.book, root, 4096, fonts, state)?
    } else {
        ReaderStartup::from_file(args.book, fonts, state)?
    };
    loop {
        let cancelled = keyboard
            .borrow_mut()
            .pressed
            .drain(..)
            .any(|event| event == InputEvent::Key(Key::Escape));
        if !window.is_open() || cancelled || window.is_key_down(Key::Escape) {
            return Ok(());
        }
        if startup.step_for(std::time::Duration::from_millis(5))? {
            break;
        }
        window.set_title(&format!(
            "Femto - Opening book - {} pages indexed - Esc: cancel",
            startup.indexed_pages()
        ));
        window.update();
    }
    let mut reader = startup.finish()?;
    reader.record_position(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
    // Chapter page indexes are kept beside the state file.
    reader.set_index_store(Some(femto_reader::page_index::IndexStore::new(
        args.state.with_extension("index"),
        env!("CARGO_PKG_VERSION"),
    )));
    drop(loading);
    let mut ui = ReaderUi::default();
    if let Some(directory) = args.dictionary_directory {
        ui.configure_dictionaries(
            femto_reader::dictionary::discover(directory)?,
            reader.state.dictionary_name.as_deref(),
        )?;
    } else {
        ui.set_dictionary_path(args.dictionary);
    }
    #[cfg(unix)]
    let mut tailnet_manager =
        args.tailnet_directory
            .zip(args.tailnet_endpoint)
            .map(|(directory, endpoint)| {
                simulator_tailnet::Manager::new(directory, endpoint, args.tailnet_ca)
                    .with_relay_options(args.tailnet_relay_ca, args.tailnet_relay_only)
                    .with_service_ca(args.tailnet_service_ca)
            });
    #[cfg(unix)]
    ui.set_tailnet_available(tailnet_manager.is_some());
    #[cfg(unix)]
    let mut kosync = args
        .kosync_config
        .as_deref()
        .map(femto_kosync::config::Settings::load)
        .transpose()?
        .map(femto_kosync::session::Session::new);
    #[cfg(unix)]
    ui.set_kosync_available(kosync.is_some());
    #[cfg(unix)]
    let tailnet = femto_catalog::tailnet::Factory::default();
    #[cfg(unix)]
    let mut catalog = args
        .catalog_directory
        .as_ref()
        .map(|path| {
            catalog_menu::CatalogMenu::load_from(path)
                .map_err(anyhow::Error::msg)?
                .ok_or_else(|| anyhow::anyhow!("catalog directory has no catalog.url"))
        })
        .transpose()?;
    #[cfg(unix)]
    let mut catalog_jobs = catalog_menu::Jobs::default();
    #[cfg(unix)]
    if let Some(catalog) = &mut catalog {
        catalog.set_transport_factory(std::sync::Arc::new(move |origins| {
            tailnet.routed(host_http::HostTransport { endpoint: None }, origins)
        }));
        ui.set_catalog_available(true);
    }
    let library = library_directory
        .as_ref()
        .map(|directory| Library::load_cached(directory, 4096))
        .transpose()?
        .unwrap_or_default();
    ui.set_library(library);
    if let Some(directory) = library_directory {
        ui.configure_library_scan(directory, 4096)?;
    }
    window.set_target_fps(30);
    let mut pixels = Vec::new();
    let mut dirty = true;
    let mut mouse_down = false;
    let mut error = None::<String>;
    let mut busy = false;
    while window.is_open() {
        if dirty {
            let frame = ui.render(&mut reader, args.levels)?;
            pixels = frame.into_iter().map(|v| u32::from(v) * 0x010101).collect();
            let title = if let Some(message) = error.as_deref().or_else(|| ui.error_message()) {
                format!("Femto - {message}")
            } else if let Some(status) = ui.kosync_status() {
                format!("Femto - KOReader sync - {status}")
            } else if ui.is_dictionary_result() {
                "Femto - Dictionary - Backspace: selection".into()
            } else if ui.is_dictionary_opening() {
                "Femto - Preparing dictionary".into()
            } else if ui.is_preview_opening() {
                "Femto - Opening link preview".into()
            } else if let Some((page, count)) = ui.preview_page() {
                format!("Femto - Link preview {page}/{count} - Enter: follow - Backspace: close")
            } else if ui.is_editing() {
                "Femto - Library search".into()
            } else if ui.is_scanning() {
                "Femto - Scanning library".into()
            } else {
                format!(
                    "Femto - {} - {} - page {}/{} - Tab: menu",
                    reader.document.metadata.title,
                    reader.section_label(),
                    reader.page_index() + 1,
                    reader.page_count()
                )
            };
            window.set_title(&title);
            dirty = false;
        }
        window.update_with_buffer(&pixels, width, height)?;
        let keys: Vec<_> = keyboard.borrow_mut().pressed.drain(..).collect();
        for event in keys {
            if event == InputEvent::Key(Key::Q) && !ui.is_editing() {
                save(&mut reader, &args.state)?;
                return Ok(());
            }
            let result = match event {
                InputEvent::Key(key) => handle_key(&mut reader, &mut ui, key),
                InputEvent::Text(character) => Ok(ui.type_char(character)),
            };
            apply_result(
                result,
                &mut reader,
                &mut ui,
                &args.state,
                &mut dirty,
                &mut error,
            )?;
        }
        let down = window.get_mouse_down(MouseButton::Left);
        if down
            && !mouse_down
            && let Some((x, y)) = window.get_mouse_pos(MouseMode::Discard)
        {
            let result = ui.tap(&mut reader, x as u32, y as u32);
            apply_result(
                result,
                &mut reader,
                &mut ui,
                &args.state,
                &mut dirty,
                &mut error,
            )?;
        }
        mouse_down = down;
        #[cfg(unix)]
        if let Some(manager) = &mut tailnet_manager {
            if let Some(event) = ui.take_tailnet_event() {
                manager.event(event, &mut ui);
                dirty = true;
            }
            dirty |= manager.poll(&mut ui);
        }
        #[cfg(unix)]
        if let Some(sync) = &mut kosync {
            if let Some(event) = ui.take_kosync_event() {
                let factory = tailnet_manager
                    .as_ref()
                    .map(|manager| manager.factory())
                    .unwrap_or_default();
                let transport = std::sync::Arc::new(move |origins: &[String]| {
                    factory.routed(host_http::HostTransport { endpoint: None }, origins)
                });
                if let Err(message) = sync.event(event, &reader, &mut ui, transport) {
                    ui.show_error(message);
                }
                dirty = true;
            }
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            dirty |= sync.poll(&mut reader, &mut ui, now, |state| state.save(&args.state));
        }
        #[cfg(unix)]
        if let Some(catalog) = &mut catalog {
            if let Some(event) = ui.take_catalog_event() {
                let factory = tailnet_manager
                    .as_ref()
                    .map(|manager| manager.factory())
                    .unwrap_or_default();
                catalog.set_transport_factory(std::sync::Arc::new(move |origins| {
                    factory.routed(host_http::HostTransport { endpoint: None }, origins)
                }));
                if let Err(message) = catalog.event(event, &mut catalog_jobs, &mut ui) {
                    ui.show_error(message);
                }
                dirty = true;
            }
            let items = ui.menu_items(&reader);
            if catalog.poll(&mut catalog_jobs, &mut ui, items) {
                dirty = true;
            }
        }
        if ui.poll_scan() == Effect::Redraw {
            dirty = true;
        }
        if ui.poll_dictionary(&mut reader) == Effect::Redraw {
            dirty = true;
        }
        if ui.poll_preview(&mut reader) == Effect::Redraw {
            dirty = true;
        }
        if ui.poll_navigation(&mut reader) == Effect::Redraw {
            dirty = true;
            save(&mut reader, &args.state)?;
        }
        if ui.poll_reflow(&mut reader) == Effect::Redraw {
            dirty = true;
            save(&mut reader, &args.state)?;
        }
        if ui.poll_open(&mut reader) == Effect::Redraw {
            dirty = true;
            save(&mut reader, &args.state)?;
        }
        // Index the rest of the chapter and the next one between inputs.
        if !ui.is_busy() {
            reader.index_step()?;
        }
        // The busy mark appears and disappears with pending work.
        if ui.is_busy() != busy {
            busy = !busy;
            dirty = true;
        }
    }
    save(&mut reader, &args.state)
}

fn handle_key(reader: &mut Reader, ui: &mut ReaderUi, key: Key) -> femto_reader::Result<Effect> {
    match key {
        Key::Backspace if ui.is_editing() => Ok(ui.delete_char()),
        Key::Enter if ui.is_editing() => Ok(ui.submit_search()),
        Key::Space if ui.is_editing() => Ok(Effect::Unchanged),
        Key::Tab | Key::Escape => {
            if ui.is_open() {
                ui.back(reader)
            } else {
                ui.open();
                Ok(Effect::Redraw)
            }
        }
        Key::Backspace => ui.back(reader),
        Key::Enter => ui.select(reader),
        Key::Right | Key::Down | Key::PageDown | Key::Space => ui.turn(reader, true),
        Key::Left | Key::Up | Key::PageUp => ui.turn(reader, false),
        Key::B if !ui.is_open() => reader.dispatch(Action::ToggleBookmark),
        Key::Equal if !ui.is_open() => ui.resize(reader, 2.0),
        Key::Minus if !ui.is_open() => ui.resize(reader, -2.0),
        _ => Ok(Effect::Unchanged),
    }
}

fn apply_result(
    result: femto_reader::Result<Effect>,
    reader: &mut Reader,
    ui: &mut ReaderUi,
    state: &PathBuf,
    dirty: &mut bool,
    error: &mut Option<String>,
) -> Result<()> {
    match result {
        Ok(effect) => {
            *error = None;
            // The simulator persists the requested level without controlling a lamp.
            ui.take_light_request();
            if effect != Effect::Unchanged {
                *dirty = true;
                save(reader, state)?;
            }
        }
        Err(cause) => {
            *error = Some(cause.to_string());
            *dirty = true;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn tailnet_arguments_require_directory_and_numeric_endpoint_together() {
        use clap::Parser;
        let parse = |extra: &[&str]| {
            super::Args::try_parse_from(
                ["femto-simulator", "book.epub", "--font", "font.ttf"]
                    .into_iter()
                    .chain(extra.iter().copied()),
            )
        };
        assert!(parse(&[]).is_ok());
        assert!(parse(&["--tailnet-ca", "/tmp/root.der"]).is_err());
        assert!(parse(&["--tailnet-relay-ca", "/tmp/root.der"]).is_err());
        assert!(parse(&["--tailnet-relay-only"]).is_err());
        assert!(parse(&["--tailnet-service-ca", "/tmp/root.der"]).is_err());
        assert!(parse(&["--tailnet-directory", "/tmp/tailnet"]).is_err());
        assert!(parse(&["--tailnet-endpoint", "127.0.0.1:443"]).is_err());
        assert!(
            parse(&[
                "--tailnet-directory",
                "/tmp/tailnet",
                "--tailnet-endpoint",
                "control.example:443"
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "--tailnet-directory",
                "/tmp/tailnet",
                "--tailnet-endpoint",
                "127.0.0.1:443"
            ])
            .is_ok()
        );
    }
    use super::*;
    use femto_reader::document::Document;

    fn complete_navigation(reader: &mut Reader, ui: &mut ReaderUi) {
        for _ in 0..16_384 {
            if !ui.is_navigating() {
                return;
            }
            ui.poll_navigation(reader);
        }
        assert!(!ui.is_navigating());
    }

    #[test]
    fn keyboard_navigation_uses_the_shared_menu_and_back_history() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        let mut reader = Reader::new(
            Document::open(fixture.join("sample.epub")).unwrap(),
            Fonts::new(Font::load(fixture.join("DejaVuSerif.ttf")).unwrap()),
            ReaderState::default(),
        )
        .unwrap();
        let mut ui = ReaderUi::default();
        handle_key(&mut reader, &mut ui, Key::Tab).unwrap();
        assert!(ui.is_open());
        let size = reader.state.layout.font_size;
        assert_eq!(
            handle_key(&mut reader, &mut ui, Key::Equal).unwrap(),
            Effect::Unchanged
        );
        assert_eq!(reader.state.layout.font_size, size);
        ui.choose(&mut reader, "This book").unwrap();
        for key in [Key::Enter, Key::Down, Key::Enter] {
            handle_key(&mut reader, &mut ui, key).unwrap();
        }
        assert!(ui.is_navigating());
        assert_eq!(reader.page().start.chapter, 0);
        handle_key(&mut reader, &mut ui, Key::Escape).unwrap();
        assert!(!ui.is_navigating());
        assert_eq!(reader.page().start.chapter, 0);
        for key in [Key::Down, Key::Enter] {
            handle_key(&mut reader, &mut ui, key).unwrap();
        }
        complete_navigation(&mut reader, &mut ui);
        assert!(!ui.is_open());
        assert_eq!(reader.page().start.chapter, 1);
        handle_key(&mut reader, &mut ui, Key::Backspace).unwrap();
        assert!(ui.is_navigating());
        assert_eq!(reader.page().start.chapter, 1);
        complete_navigation(&mut reader, &mut ui);
        assert_eq!(reader.page().start.chapter, 0);
        ui.tap(&mut reader, 240, 400).unwrap();
        assert!(ui.is_open());
        handle_key(&mut reader, &mut ui, Key::Escape).unwrap();
        assert!(!ui.is_open());
    }

    #[test]
    fn short_key_presses_survive_frame_boundaries_in_order() {
        let events = Rc::new(RefCell::new(KeyEvents::default()));
        let mut callback = Keyboard(events.clone());
        callback.set_key_state(Key::Right, true);
        callback.set_key_state(Key::Right, true);
        callback.set_key_state(Key::Right, false);
        callback.set_key_state(Key::B, true);
        callback.set_key_state(Key::B, false);
        assert_eq!(
            events.borrow().pressed.iter().copied().collect::<Vec<_>>(),
            [InputEvent::Key(Key::Right), InputEvent::Key(Key::B)]
        );
        assert!(events.borrow().held.is_empty());
        for _ in 0..100 {
            callback.set_key_state(Key::Left, true);
            callback.set_key_state(Key::Left, false);
        }
        assert_eq!(events.borrow().pressed.len(), 64);
    }
}
