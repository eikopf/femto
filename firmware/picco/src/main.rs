use femto_picco_board::input::Turn;
use femto_reader::{
    controller::{Effect, Reader, ReaderStartup},
    document::Document,
    journal::StateJournal,
    layout::{Font, Fonts},
    state::{Library, LibraryScan, ReaderState},
    ui::ReaderUi,
};
use std::time::{Duration, Instant};
#[cfg(feature = "bluetooth")]
mod bluetooth;
mod catalog;
mod clock;
pub mod control;
pub mod identity;
mod input;
mod light_command;
pub mod network;
mod panel;
mod power;
pub mod relay;
pub mod relay_transport;
mod storage;
pub mod tailnet;
mod tailnet_http;
mod tailnet_menu;
mod tailnet_rotation;
mod tailnet_tls;
mod wifi;

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    if let Err(error) = run() {
        log::error!("Reader stopped: {error}");
    }
    loop {
        std::thread::sleep(Duration::from_secs(10));
    }
}

fn run() -> Result<(), String> {
    let mut screen = panel::open()?;
    let default_font =
        || Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec());
    let fonts = if screen.storage.is_some() {
        let loaded = femto_reader::fontset::load(
            std::path::Path::new("/sdcard/femto/fonts"),
            default_font,
            4 * 1024 * 1024,
        )
        .map_err(|e| e.to_string())?;
        for warning in &loaded.warnings {
            log::warn!("Font: {warning}");
        }
        log::info!("Loaded {} bytes of SD font data", loaded.bytes);
        loaded.fonts
    } else {
        Fonts::new(default_font().map_err(|e| e.to_string())?)
    };
    let mut candidates = Vec::new();
    let mut journal = None;
    let mut state = ReaderState::default();
    if screen.storage.is_some() {
        match StateJournal::open("/sdcard/femto") {
            Ok((store, saved)) => {
                journal = Some(store);
                state = saved;
            }
            Err(error) => log::error!("State unavailable; saving disabled: {error}"),
        }
    }
    // Device geometry is fixed even if a state file came from a host viewport.
    let (width, height) = orientation(state.rotation).dimensions();
    state.layout.width = width as u32;
    state.layout.height = height as u32;
    let library = if screen.storage.is_some() {
        startup_library(&mut screen, &fonts, &state)?
    } else {
        Library::default()
    };
    if unsafe { esp_idf_svc::sys::esp_sleep_get_wakeup_cause() }
        != esp_idf_svc::sys::esp_sleep_source_t_ESP_SLEEP_WAKEUP_UNDEFINED
    {
        if let Some(entry) = library.recent(&state).first() {
            candidates.push((entry.path.clone(), true));
        }
    }
    if screen.storage.is_some() {
        for path in ["/sdcard/femto/book.epub", "/sdcard/femto/book.txt"] {
            let path = std::path::PathBuf::from(path);
            if !candidates.iter().any(|(existing, _)| existing == &path) {
                candidates.push((path, false));
            }
        }
    }
    let (mut reader, resumed) = startup_reader(&mut screen, fonts, state, candidates)?;
    let mut ui = ReaderUi::default();
    if screen.storage.is_some() {
        let path = std::path::PathBuf::from("/sdcard/femto/dictionary.ifo");
        let mut paths = match femto_reader::dictionary::discover("/sdcard/femto/dictionaries") {
            Ok(paths) => paths,
            Err(error) => {
                log::warn!("Dictionary discovery failed: {error}");
                Vec::new()
            }
        };
        if path.is_file() {
            paths.push(path.clone());
        }
        if let Err(error) =
            ui.configure_dictionaries(paths, reader.state.dictionary_name.as_deref())
        {
            log::warn!("Dictionary configuration failed: {error}");
            if path.is_file() {
                ui.set_dictionary_path(Some(path));
            }
        }
    }
    let mut catalog = match catalog::CatalogMenu::load() {
        Ok(catalog) => catalog,
        Err(error) => {
            log::warn!("Catalog unavailable: {error}");
            None
        }
    };
    ui.set_catalog_available(catalog.is_some());
    let mut kosync = match femto_kosync::config::Settings::load(std::path::Path::new(
        "/sdcard/femto/kosync.json",
    )) {
        Ok(settings) => Some(femto_kosync::session::Session::new(settings)),
        Err(femto_kosync::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            log::warn!("KOReader configuration unavailable: {error}");
            None
        }
    };
    ui.set_kosync_available(kosync.is_some());
    ui.set_tailnet_available(true);
    ui.set_rotation_available(true);
    ui.set_device_status(&format!(
        "{} | {}",
        battery_label(screen.battery()),
        screen.wifi_label()
    ));
    ui.set_sleep_available(journal.is_some());
    let choose_book = !resumed && !library.entries.is_empty();
    ui.set_library(library);
    if screen.storage.is_some() {
        ui.configure_library_scan("/sdcard/femto".into(), 256)
            .map_err(|e| e.to_string())?;
    }
    if choose_book {
        ui.open_library();
    }
    if let Err(error) = screen.set_light(
        reader.state.frontlight.brightness,
        reader.state.frontlight.warmth,
    ) {
        log::warn!("{error}");
        ui.open();
        ui.show_error(error);
    }
    let mut plane =
        vec![255; femto_picco_board::PLANE_BYTES * if cfg!(feature = "grayscale") { 2 } else { 1 }];
    render(&mut reader, &ui, &mut plane)?;
    screen.present_oriented(&plane, orientation(reader.state.rotation))?;
    log::info!(
        "Reader ready: {} pages; GPIO21 previous, GPIO9 next",
        reader.page_count()
    );
    let idle_epoch = Instant::now();
    let mut idle =
        femto_picco_board::idle::Idle::new(0, reader.state.idle_sleep.minutes() * 60 * 1000);
    let mut heap_report = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(10));
        screen.poll_wifi();
        let event = screen.event().or_else(|| {
            screen
                .remote_turn(&reader.state, !ui.is_open())
                .map(input::Event::Turn)
        });
        if let Some(event) = event {
            idle.activity(idle_epoch.elapsed().as_millis().min(u64::MAX as u128) as u64);
            ui.set_device_status(&format!(
                "{} | {}",
                battery_label(screen.battery()),
                screen.wifi_label()
            ));
            let previous_light = reader.state.frontlight;
            let effect = match event {
                input::Event::Select => ui.select(&mut reader),
                input::Event::Back => ui.back(&mut reader),
                input::Event::Turn(Turn::Previous) => ui.turn(&mut reader, false),
                input::Event::Turn(Turn::Next) => ui.turn(&mut reader, true),
                input::Event::Tap { x, y } => ui.tap(&mut reader, x.into(), y.into()),
            };
            let mut effect = effect.map_err(|e| e.to_string())?;
            if let Some(rotation) = ui.take_rotation_request() {
                if let Err(error) = ui.rotate(&reader, rotation) {
                    ui.show_error(error.to_string());
                }
                effect = Effect::Redraw;
            }
            if let Some(event) = ui.take_tailnet_event() {
                if let Err(error) = screen.tailnet.event(event, &mut ui) {
                    ui.show_error(error);
                }
                effect = Effect::Redraw;
            }
            if let Some(event) = ui.take_catalog_event() {
                if let Some(catalog) = &mut catalog {
                    let driver = screen.tailnet.driver().map(|driver| driver.downgrade());
                    catalog.set_transport_factory(std::sync::Arc::new(move |origins| {
                        let refs: Vec<_> = origins.iter().map(String::as_str).collect();
                        Ok(Box::new(femto_catalog::routing::Routed::new(
                            network::Transport,
                            tailnet_http::Transport(
                                driver.as_ref().and_then(|driver| driver.upgrade()),
                            ),
                            &refs,
                        )?))
                    }));
                    if let Err(error) = catalog.event(event, &mut screen.catalog, &mut ui) {
                        ui.show_error(error);
                    }
                    effect = Effect::Redraw;
                }
            }
            if let (Some(event), Some(sync)) = (ui.take_kosync_event(), kosync.as_mut()) {
                let driver = screen.tailnet.driver().map(|driver| driver.downgrade());
                let transport = std::sync::Arc::new(move |origins: &[String]| {
                    let refs: Vec<_> = origins.iter().map(String::as_str).collect();
                    Ok(Box::new(femto_catalog::routing::Routed::new(
                        network::Transport,
                        tailnet_http::Transport(
                            driver.as_ref().and_then(|driver| driver.upgrade()),
                        ),
                        &refs,
                    )?)
                        as Box<dyn femto_catalog::http::Transport>)
                });
                if let Err(error) = sync.event(event, &reader, &mut ui, transport) {
                    ui.show_error(error);
                }
                effect = Effect::Redraw;
            }
            if effect == Effect::Redraw {
                if ui.take_light_request() {
                    let light = reader.state.frontlight;
                    if let Err(error) = screen.set_light(light.brightness, light.warmth) {
                        log::warn!("{error}");
                        reader.state.frontlight = previous_light;
                        ui.show_error(error);
                    }
                }
                render(&mut reader, &ui, &mut plane)?;
                screen.present(&plane)?;
                if !ui.is_open() {
                    // Monotonic access order until RTC/network time is available.
                    let access = reader
                        .state
                        .books
                        .values()
                        .map(|book| book.last_opened)
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1);
                    reader.record_position(access);
                    if let Some(store) = &mut journal
                        && let Err(error) = store.save(&reader.state)
                    {
                        log::error!("State save failed; saving disabled: {error}");
                        journal = None;
                        ui.set_sleep_available(false);
                    }
                }
                log::info!("Page {}/{}", reader.page_index() + 1, reader.page_count());
            }
        }
        if catalog
            .as_mut()
            .is_some_and(|catalog| catalog.poll(&mut screen.catalog, &mut ui))
        {
            render(&mut reader, &ui, &mut plane)?;
            screen.present(&plane)?;
        }
        if screen.tailnet.poll(&mut ui) {
            render(&mut reader, &ui, &mut plane)?;
            screen.present(&plane)?;
        }
        if let Some(sync) = &mut kosync {
            let access = reader
                .state
                .books
                .values()
                .map(|book| book.last_opened)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            if sync.poll(&mut reader, &mut ui, access, |state| {
                journal
                    .as_mut()
                    .ok_or_else(|| std::io::Error::other("state journal unavailable"))?
                    .save(state)
            }) {
                render(&mut reader, &ui, &mut plane)?;
                screen.present(&plane)?;
            }
        }
        if ui.poll_scan() == Effect::Redraw {
            render(&mut reader, &ui, &mut plane)?;
            screen.present(&plane)?;
        }
        let navigated = ui.poll_navigation(&mut reader) == Effect::Redraw;
        if ui.poll_open(&mut reader) == Effect::Redraw || navigated {
            if !ui.is_open() {
                let access = reader
                    .state
                    .books
                    .values()
                    .map(|book| book.last_opened)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1);
                reader.record_position(access);
                if let Some(store) = &mut journal
                    && let Err(error) = store.save(&reader.state)
                {
                    log::error!("State save failed; saving disabled: {error}");
                    journal = None;
                    ui.set_sleep_available(false);
                }
            }
            render(&mut reader, &ui, &mut plane)?;
            screen.present(&plane)?;
        }
        if ui.poll_dictionary(&mut reader) == Effect::Redraw {
            render(&mut reader, &ui, &mut plane)?;
            screen.present(&plane)?;
        }
        if ui.poll_preview(&mut reader) == Effect::Redraw {
            render(&mut reader, &ui, &mut plane)?;
            screen.present(&plane)?;
        }
        let previous_rotation = reader.state.rotation;
        if ui.poll_reflow(&mut reader) == Effect::Redraw {
            render(&mut reader, &ui, &mut plane)?;
            if reader.state.rotation != previous_rotation {
                screen.present_oriented(&plane, orientation(reader.state.rotation))?;
            } else {
                screen.present(&plane)?;
            }
            reader.record_position(
                reader
                    .state
                    .books
                    .get(&reader.document.metadata.id)
                    .map_or(0, |book| book.last_opened),
            );
            if let Some(store) = &mut journal
                && let Err(error) = store.save(&reader.state)
            {
                log::error!("State save failed; saving disabled: {error}");
                journal = None;
                ui.set_sleep_available(false);
                ui.show_error("Layout applied; save failed".into());
                render(&mut reader, &ui, &mut plane)?;
                screen.present(&plane)?;
            }
        }
        let manual_sleep = ui.take_sleep_request();
        let idle_now = idle_epoch.elapsed().as_millis().min(u64::MAX as u128) as u64;
        idle.configure(idle_now, reader.state.idle_sleep.minutes() * 60 * 1000);
        let automatic_sleep = idle.poll(
            idle_now,
            journal.is_some(),
            ui.is_scanning()
                || ui.is_opening()
                || ui.is_reflowing()
                || ui.is_navigating()
                || ui.is_preview_opening()
                || ui.is_dictionary_opening()
                || screen.catalog.is_busy()
                || kosync.as_ref().is_some_and(|sync| sync.is_busy()),
        );
        if manual_sleep || automatic_sleep {
            idle.suppress_until_activity();
            let prepared = (|| -> Result<(), String> {
                power::wait_for_release()?;
                if kosync
                    .as_mut()
                    .is_some_and(|sync| !sync.cancel_and_wait(Duration::from_secs(5)))
                {
                    return Err("KOReader request is still stopping".into());
                }
                let store = journal
                    .as_mut()
                    .ok_or_else(|| "Sleep requires a writable state journal".to_string())?;
                let access = reader
                    .state
                    .books
                    .values()
                    .map(|book| book.last_opened)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1);
                reader.record_position(access);
                store
                    .save(&reader.state)
                    .map_err(|e| format!("Sleep save failed: {e}"))?;
                ui.release_dictionary();
                screen.prepare_sleep()
            })();
            match prepared {
                Ok(()) => {
                    drop(ui);
                    drop(reader);
                    drop(journal);
                    drop(screen);
                    power::enter();
                }
                Err(error) => {
                    log::warn!("{error}");
                    let light = reader.state.frontlight;
                    if let Err(restore) = screen.set_light(light.brightness, light.warmth) {
                        log::warn!("Light restore failed: {restore}");
                    }
                    ui.open();
                    ui.show_error(error);
                    render(&mut reader, &ui, &mut plane)?;
                    screen.present(&plane)?;
                }
            }
        }
        if heap_report.elapsed() >= Duration::from_secs(10) {
            let free = unsafe { esp_idf_svc::sys::esp_get_free_heap_size() };
            log::info!("free heap: {free} bytes; battery: {:?}", screen.battery());
            heap_report = Instant::now();
        }
    }
}

fn startup_library(
    screen: &mut panel::Screen,
    fonts: &Fonts,
    state: &ReaderState,
) -> Result<Library, String> {
    let mut scan = match LibraryScan::cached("/sdcard/femto", 256) {
        Ok(scan) => scan,
        Err(error) => {
            log::warn!("Library scan unavailable: {error}");
            return Ok(Library::default());
        }
    };
    if let Err(error) = screen.set_light(state.frontlight.brightness, state.frontlight.warmth) {
        log::warn!("Startup frontlight unavailable: {error}");
    }
    let config = femto_reader::layout::LayoutConfig {
        width: state.layout.width,
        height: state.layout.height,
        ..Default::default()
    };
    let mut message = Document::from_text(
        "Scanning library",
        "Scanning library\n\nPress a button or tap to skip.\n\nThe library menu can rescan later.",
    )
    .map_err(|e| e.to_string())?;
    let chapter = message.chapter(0).map_err(|e| e.to_string())?;
    let pages =
        femto_reader::layout::paginate(&chapter, fonts, config).map_err(|e| e.to_string())?;
    let pixels = femto_reader::layout::rasterize_display(
        pages.pages.first().ok_or("Missing scan status page")?,
        fonts,
        config,
        state.text_raster,
        if cfg!(feature = "grayscale") { 4 } else { 2 },
        |_| {
            Err(femto_reader::Error::Document(
                "Scan status has no resources".into(),
            ))
        },
    )
    .map_err(|e| e.to_string())?;
    let mut plane =
        vec![255; femto_picco_board::PLANE_BYTES * if cfg!(feature = "grayscale") { 2 } else { 1 }];
    pack_pixels(&pixels, state.rotation, &mut plane)?;
    drop(pixels);
    screen.present_oriented(&plane, orientation(state.rotation))?;
    drop(plane);
    drop(pages);
    drop(chapter);
    drop(message);
    loop {
        std::thread::sleep(Duration::from_millis(10));
        screen.poll_wifi();
        if screen.event().is_some() {
            log::info!("Startup library scan skipped; cached index preserved");
            return Ok(Library::default());
        }
        match scan.step() {
            Ok(Some(library)) => {
                log::info!(
                    "Library: {} books, {} errors",
                    library.entries.len(),
                    library.errors.len()
                );
                return Ok(library);
            }
            Ok(None) => {}
            Err(error) => {
                log::warn!("Library scan failed: {error}");
                return Ok(Library::default());
            }
        }
    }
}

fn startup_reader(
    screen: &mut panel::Screen,
    fonts: Fonts,
    state: ReaderState,
    candidates: Vec<(std::path::PathBuf, bool)>,
) -> Result<(Reader, bool), String> {
    let specimen = || {
        Document::from_text(
            "Femto reader specimen",
            include_str!("../../../tests/fixtures/device-reader.txt"),
        )
    };
    let rotation = state.rotation;
    if let Err(error) = screen.set_light(state.frontlight.brightness, state.frontlight.warmth) {
        log::warn!("Startup frontlight unavailable: {error}");
    }
    let mut startup =
        ReaderStartup::from_document(specimen().map_err(|e| e.to_string())?, fonts, state)
            .map_err(|e| e.to_string())?;
    let levels = if cfg!(feature = "grayscale") { 4 } else { 2 };
    let mut plane =
        vec![255; femto_picco_board::PLANE_BYTES * if cfg!(feature = "grayscale") { 2 } else { 1 }];
    'books: for (path, resumed) in candidates {
        if let Err(error) = startup.open_file(&path) {
            log::warn!("Cannot open {}: {error}", path.display());
            continue;
        }
        let pixels = startup.loading_frame(levels).map_err(|e| e.to_string())?;
        pack_pixels(&pixels, rotation, &mut plane)?;
        drop(pixels);
        screen.present_oriented(&plane, orientation(rotation))?;
        let mut refreshed = Instant::now();
        let mut displayed_pages = 0;
        loop {
            std::thread::sleep(Duration::from_millis(10));
            screen.poll_wifi();
            if screen.event().is_some() {
                log::info!("Startup opening cancelled");
                break 'books;
            }
            match startup.step() {
                Ok(true) => {
                    return startup
                        .finish()
                        .map(|reader| (reader, resumed))
                        .map_err(|e| e.to_string());
                }
                Ok(false) => {}
                Err(error) => {
                    log::warn!("Cannot load {}: {error}", path.display());
                    continue 'books;
                }
            }
            if startup.indexed_pages() != displayed_pages
                && refreshed.elapsed() >= Duration::from_secs(2)
            {
                let pixels = startup.loading_frame(levels).map_err(|e| e.to_string())?;
                pack_pixels(&pixels, rotation, &mut plane)?;
                screen.present(&plane)?;
                displayed_pages = startup.indexed_pages();
                refreshed = Instant::now();
            }
        }
    }
    let (fonts, state) = startup.cancel();
    let mut fallback =
        ReaderStartup::from_document(specimen().map_err(|e| e.to_string())?, fonts, state)
            .map_err(|e| e.to_string())?;
    loop {
        std::thread::sleep(Duration::from_millis(10));
        screen.poll_wifi();
        let _ = screen.event();
        if fallback.step().map_err(|e| e.to_string())? {
            return fallback
                .finish()
                .map(|reader| (reader, false))
                .map_err(|e| e.to_string());
        }
    }
}

fn render(reader: &mut Reader, ui: &ReaderUi, plane: &mut [u8]) -> Result<(), String> {
    let pixels = ui
        .render(reader, if cfg!(feature = "grayscale") { 4 } else { 2 })
        .map_err(|e| e.to_string())?;
    pack_pixels(&pixels, reader.state.rotation, plane)
}

fn pack_pixels(
    pixels: &[u8],
    rotation: femto_reader::state::Rotation,
    plane: &mut [u8],
) -> Result<(), String> {
    if cfg!(feature = "grayscale") {
        let (msb, lsb) = plane.split_at_mut(femto_picco_board::PLANE_BYTES);
        orientation(rotation)
            .grayscale(pixels, msb, lsb)
            .map_err(|e| format!("{e:?}"))
    } else {
        orientation(rotation)
            .monochrome(pixels, plane)
            .map_err(|e| format!("{e:?}"))
    }
}

fn battery_label(sample: Option<femto_picco_board::battery::Sample>) -> String {
    use femto_picco_board::battery::Power;
    let Some(sample) = sample else {
        return "Battery unavailable".into();
    };
    let voltage = sample
        .millivolts
        .map(|mv| format!("{mv}mV"))
        .unwrap_or_else(|| "voltage?".into());
    let percentage = sample
        .percentage
        .map(|p| format!("~{p}%"))
        .unwrap_or_else(|| "?%".into());
    let power = match sample.power {
        None => "power?",
        Some(Power::Battery) => "battery",
        Some(Power::Charging) => "charging",
        Some(Power::Complete) => "complete",
        Some(Power::External) => "external",
    };
    format!("Battery {percentage} {voltage} {power}")
}

fn orientation(
    rotation: femto_reader::state::Rotation,
) -> femto_picco_board::orientation::Orientation {
    use femto_picco_board::orientation::Orientation;
    match rotation {
        femto_reader::state::Rotation::Portrait => Orientation::Portrait,
        femto_reader::state::Rotation::Clockwise => Orientation::Clockwise,
        femto_reader::state::Rotation::Inverted => Orientation::Inverted,
        femto_reader::state::Rotation::Counterclockwise => Orientation::Counterclockwise,
    }
}
