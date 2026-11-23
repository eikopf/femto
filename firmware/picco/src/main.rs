use femto_picco_board::input::Turn;
use femto_reader::{
    controller::{Effect, Reader, ReaderStartup},
    document::Document,
    journal::StateJournal,
    layout::{Font, Fonts},
    state::{Library, ReaderState},
    ui::ReaderUi,
};
use std::time::{Duration, Instant};
#[cfg(not(esp_idf_mbedtls_hardware_sha))]
compile_error!("Picco book hashing requires CONFIG_MBEDTLS_HARDWARE_SHA=y");
#[cfg(feature = "bluetooth")]
mod bluetooth;
mod catalog;
mod clock;
mod console;
pub mod control;
mod crash;
pub mod identity;
mod input;
mod light_command;
pub mod network;
mod nvs;
mod panel;
mod power;
pub mod relay;
pub mod relay_transport;
mod services;
mod setup;
mod storage;
pub mod tailnet;
mod tailnet_http;
mod tailnet_menu;
mod tailnet_rotation;
mod tailnet_tls;
mod threads;
mod update;
mod wifi;

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::info!("Book SHA-256 backend: {}", femto_reader::hashing::BACKEND);
    report_heap("boot");
    crash::install();
    if let Err(error) = run() {
        log::error!("Reader stopped: {error}");
        crash::stopped(&error);
    }
    loop {
        std::thread::sleep(Duration::from_secs(10));
    }
}

fn report_heap(stage: &str) {
    use esp_idf_svc::sys;
    let stack_free = unsafe { sys::uxTaskGetStackHighWaterMark(core::ptr::null_mut()) };
    log::info!("Stack {stage}: minimum_free={stack_free}");
    for (kind, caps) in [
        ("internal", sys::MALLOC_CAP_INTERNAL | sys::MALLOC_CAP_8BIT),
        ("psram", sys::MALLOC_CAP_SPIRAM | sys::MALLOC_CAP_8BIT),
    ] {
        let (free, largest, minimum) = unsafe {
            (
                sys::heap_caps_get_free_size(caps),
                sys::heap_caps_get_largest_free_block(caps),
                sys::heap_caps_get_minimum_free_size(caps),
            )
        };
        log::info!("Heap {stage} {kind}: free={free} largest={largest} minimum={minimum}");
    }
}

/// Names the book being opened at startup until its first page is shown.
const OPENING_MARKER: &str = "/sdcard/femto/.opening";

fn run() -> Result<(), String> {
    let mut screen = panel::open()?;
    crash::note_restart();
    // The input task above keeps its internal stack; workers started from
    // here on get theirs in external RAM.
    threads::use_external_stacks()?;
    if screen.storage.is_some() {
        services::import_from_card();
    }
    report_heap("peripherals");
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
    let fonts = load_fonts(screen.storage.is_some(), state.font_family.as_deref())?;
    // Device geometry is fixed even if a state file came from a host viewport.
    let (width, height) = orientation(state.rotation).dimensions();
    state.layout.width = width as u32;
    state.layout.height = height as u32;
    if let Err(error) = screen.set_light(state.frontlight.brightness, state.frontlight.warmth) {
        log::warn!("Startup frontlight unavailable: {error}");
    }
    let mut library = if screen.storage.is_some() {
        match Library::load_cached("/sdcard/femto", 256) {
            Ok(library) => library,
            Err(error) => {
                log::warn!("Library cache unavailable: {error}");
                Library::default()
            }
        }
    } else {
        Library::default()
    };
    if let Some(entry) = library.recent(&state).first() {
        candidates.push((entry.path.clone(), true));
    }
    if screen.storage.is_some() {
        for path in ["/sdcard/femto/book.epub", "/sdcard/femto/book.txt"] {
            let path = std::path::PathBuf::from(path);
            if !candidates.iter().any(|(existing, _)| existing == &path) {
                candidates.push((path, false));
            }
        }
    }
    // A marker left by the previous start means its book never finished
    // opening, most likely because it reset the device. Skip that book once
    // so one bad file cannot keep the reader from starting.
    let failed_open = std::fs::read_to_string(OPENING_MARKER).ok();
    if let Some(path) = &failed_open {
        log::warn!("Previous start did not finish opening {path}; skipping it");
        candidates.retain(|(candidate, _)| candidate.as_os_str() != path.as_str());
        let _ = std::fs::remove_file(OPENING_MARKER);
    }
    report_heap("before layout");
    // Chapter page indexes are recorded per firmware build, because layout
    // code may differ between builds.
    let index_store = screen.storage.is_some().then(|| {
        let build = unsafe { (*esp_idf_svc::sys::esp_app_get_description()).app_elf_sha256 };
        let salt: String = build.iter().map(|byte| format!("{byte:02x}")).collect();
        femto_reader::page_index::IndexStore::new("/sdcard/femto/.index", &salt)
    });
    let (mut reader, resumed) = startup_reader(&mut screen, fonts, state, candidates, index_store)?;
    report_heap("reader ready");
    if screen.storage.is_some()
        && !library
            .entries
            .iter()
            .any(|entry| entry.metadata.id == reader.document.metadata.id)
    {
        match Library::load_cached("/sdcard/femto", 256) {
            Ok(updated) => library = updated,
            Err(error) => log::warn!("Library cache refresh unavailable: {error}"),
        }
    }
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
    let catalog_settings = services::Catalog::load();
    let sync_settings = services::Kosync::load();
    // A service routed through the tailnet opens the session when it is used.
    let (catalog_tailnet, sync_tailnet) = (
        catalog_settings.uses_tailnet(),
        sync_settings.uses_tailnet(),
    );
    let mut catalog = match catalog_settings.menu() {
        Ok(catalog) => catalog,
        Err(error) => {
            log::warn!("Catalog unavailable: {error}");
            None
        }
    };
    ui.set_catalog_available(catalog.is_some());
    // Sync is offered once an account is set; an incomplete draft is not an error.
    let mut kosync = sync_settings
        .settings()
        .ok()
        .map(femto_kosync::session::Session::new);
    ui.set_kosync_available(kosync.is_some());
    ui.set_tailnet_available(true);
    ui.set_rotation_available(true);
    ui.set_page_buttons_available(true);
    ui.set_gestures_available(true);
    ui.set_refresh_available(true);
    ui.set_device_available(true);
    ui.set_battery(screen.battery().and_then(|sample| sample.percentage));
    ui.set_device_status(&format!(
        "{} | {}",
        battery_label(screen.battery()),
        screen.wifi_label()
    ));
    ui.set_sleep_available(journal.is_some());
    ui.set_firmware(&update::running());
    if screen.storage.is_some() {
        ui.set_font_families(femto_reader::fontset::families(std::path::Path::new(FONTS)));
    }
    let mut updater = update::Updater::new();
    // One automatic check per start, once the network and clock allow it.
    let mut update_checked = false;
    let mut update_drawn = Instant::now();
    ui.set_update(Some(updater.view()));
    let choose_book = !resumed && !library.entries.is_empty();
    ui.set_library(library);
    if screen.storage.is_some() {
        ui.configure_library_scan("/sdcard/femto".into(), 256)
            .map_err(|e| e.to_string())?;
    }
    if choose_book || failed_open.is_some() {
        ui.open_library();
    }
    if failed_open.is_some() {
        ui.show_error("The last book did not open".into());
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
    screen.set_full_refresh_interval(reader.state.full_refresh_interval);
    screen.present_oriented(&plane, orientation(reader.state.rotation))?;
    // The book opened and its page is on the panel.
    let _ = std::fs::remove_file(OPENING_MARKER);
    log::info!(
        "Reader ready: {} pages; GPIO21 previous, GPIO9 next",
        reader.page_count()
    );
    let idle_epoch = Instant::now();
    let mut idle =
        femto_picco_board::idle::Idle::new(0, reader.state.idle_sleep.minutes() * 60 * 1000);
    let mut heap_report = Instant::now();
    let mut console = console::Console::open();
    let (mut turning_forward, mut preparing) = (true, true);
    let mut stay_awake = false;
    // A sleep asked for over USB, and how long it lasts.
    let mut timed_sleep = None::<Duration>;
    let (mut carry, mut dirty) = (None::<input::Event>, false);
    let mut setup = setup::Setup::default();
    let mut last_input = Instant::now();
    let mut busy_since: Option<(Instant, bool)> = None;
    // Keeps the radio awake while network work is in flight, and while a
    // menu is open, because a request starts from one.
    macro_rules! attend {
        () => {
            let networking = ui.is_open()
                || screen.catalog.is_busy()
                || updater.is_busy()
                || screen.tailnet.driver().is_some()
                || kosync.as_ref().is_some_and(|sync| sync.is_busy());
            if let Some(wifi) = screen.wifi() {
                wifi.set_attentive(networking);
            }
        };
    }
    // How sync requests reach their server: directly, or through the tailnet
    // for the origins the settings route there.
    macro_rules! sync_transport {
        () => {{
            let session = screen.tailnet.session();
            std::sync::Arc::new(move |origins: &[String]| {
                let refs: Vec<_> = origins.iter().map(String::as_str).collect();
                Ok(Box::new(femto_catalog::routing::Routed::new(
                    network::Transport,
                    tailnet_http::Transport(session.clone()),
                    &refs,
                )?) as Box<dyn femto_catalog::http::Transport>)
            })
        }};
    }
    // The book automatic sync has compared with the server since it opened.
    let mut reconciled = None::<String>;
    // A sleep that waits, behind the sleep page already shown, for the
    // reading position to reach the sync server: when the timer should wake
    // the reader, and a checksum of that page.
    let mut retiring = None::<(Option<Duration>, u32)>;
    loop {
        std::thread::sleep(Duration::from_millis(10));
        screen.poll_wifi();
        // A carried event is handled before any new console command is read.
        let command = if carry.is_some() {
            None
        } else {
            console.poll()
        };
        let mut chosen = String::new();
        let mut update_now = false;
        let diagnostic_event = command.and_then(|command| {
            use femto_picco_board::console::Command;
            // Typed text may be a credential.
            match &command {
                Command::Type(_) => log::info!("USB command: Type"),
                command => log::info!("USB command: {command:?}"),
            }
            match command {
                Command::Status => {
                    report_heap("usb status");
                    log::info!(
                        "USB status: page={} pages={} menu={}",
                        reader.page_index(),
                        reader.page_count(),
                        ui.is_open()
                    );
                    None
                }
                Command::Next => Some(input::Event::Turn(Turn::Next)),
                Command::Previous => Some(input::Event::Turn(Turn::Previous)),
                Command::Select => Some(input::Event::Select),
                Command::Back => Some(input::Event::Back),
                Command::Power => Some(input::Event::Power),
                Command::Awake => {
                    stay_awake = true;
                    log::info!("USB awake: automatic sleep suppressed until restart");
                    None
                }
                Command::Sleep(seconds) => {
                    timed_sleep = Some(Duration::from_secs(seconds.into()));
                    None
                }
                Command::Fault => panic!("requested over USB"),
                Command::Relay => {
                    tailnet::RELAY_ONLY.store(true, std::sync::atomic::Ordering::Relaxed);
                    log::info!("USB relay: new tailnet sessions use relays only until restart");
                    None
                }
                Command::Swipe(turn) => Some(input::Event::Swipe(turn)),
                Command::Slide { up } => Some(input::Event::Gesture(if up {
                    femto_reader::ui::Gesture::SwipeUp
                } else {
                    femto_reader::ui::Gesture::SwipeDown
                })),
                Command::Hold => Some(input::Event::Gesture(femto_reader::ui::Gesture::Hold)),
                Command::Tap { x, y } => Some(input::Event::Tap { x, y }),
                Command::Type(text) => {
                    for character in text.chars() {
                        ui.type_char(character);
                    }
                    dirty = true;
                    None
                }
                Command::Choose(label) => {
                    chosen = label;
                    Some(input::Event::Choose)
                }
                Command::Frame => {
                    console::dump_frame(&plane);
                    None
                }
                Command::Reboot => unsafe { esp_idf_svc::sys::esp_restart() },
                Command::Put { .. } | Command::List(_) | Command::Show(_) | Command::Remove(_) => {
                    console.files(&command);
                    updater.rescan();
                    ui.set_update(Some(updater.view()));
                    None
                }
                Command::Update => {
                    update_now = true;
                    None
                }
            }
        });
        let event = carry
            .take()
            .or(diagnostic_event)
            .or_else(|| screen.event())
            .or_else(|| {
                screen
                    .remote_turn(&reader.state, !ui.is_open())
                    .map(input::Event::Turn)
            });
        // A press while the position is still being sent wakes the reader.
        if retiring.is_some() && event.is_some() {
            retiring = None;
            if let Some(sync) = kosync.as_mut() {
                sync.cancel_and_wait(Duration::ZERO);
            }
            render(&mut reader, &ui, &mut plane)?;
            screen.present(&plane)?;
            continue;
        }
        // Text typed over USB changes the view without an input event.
        if event.is_none() && dirty {
            dirty = false;
            render(&mut reader, &ui, &mut plane)?;
            screen.present(&plane)?;
        }
        if let Some(event) = event {
            idle.activity(idle_epoch.elapsed().as_millis().min(u64::MAX as u128) as u64);
            ui.set_battery(screen.battery().and_then(|sample| sample.percentage));
            ui.set_device_status(&format!(
                "{} | {}{}",
                battery_label(screen.battery()),
                screen.wifi_label(),
                updater
                    .notice()
                    .map_or_else(String::new, |notice| format!(" | {notice}"))
            ));
            let previous_light = reader.state.frontlight;
            let received = Instant::now();
            let effect = match event {
                input::Event::Power => ui.power(&mut reader),
                input::Event::Select => ui.select(&mut reader),
                input::Event::Back => ui.back(&mut reader),
                input::Event::Button(turn) => {
                    // By default the upper button turns pages forward and moves a
                    // menu selection up; each arrangement can be swapped.
                    let upper = turn == Turn::Previous;
                    let next = if ui.turns_pages() {
                        upper != reader.state.swap_page_buttons
                    } else {
                        upper == reader.state.swap_menu_buttons
                    };
                    ui.turn(&mut reader, next)
                }
                input::Event::Swipe(turn) => ui.swipe(&mut reader, turn == Turn::Next),
                input::Event::Gesture(gesture) => ui.gesture(&mut reader, gesture),
                input::Event::Turn(turn) => ui.turn(&mut reader, turn == Turn::Next),
                input::Event::Tap { x, y } => ui.tap(&mut reader, x.into(), y.into()),
                input::Event::Choose => ui.choose(&mut reader, &chosen),
            };
            screen.set_long_presses(ui.is_open());
            screen.set_full_refresh_interval(reader.state.full_refresh_interval);
            let mut effect = match effect {
                Ok(effect) => effect,
                Err(error) => {
                    log::warn!("Input failed: {error}");
                    ui.open();
                    ui.show_error(error.to_string());
                    Effect::Redraw
                }
            };
            if let Some(rotation) = ui.take_rotation_request() {
                if let Err(error) = ui.rotate(&reader, rotation) {
                    ui.show_error(error.to_string());
                }
                effect = Effect::Redraw;
            }
            if let Some(family) = ui.take_font_request() {
                let started = load_fonts(true, family.as_deref()).and_then(|fonts| {
                    ui.set_fonts(&reader, fonts, family)
                        .map_err(|e| e.to_string())
                });
                if let Err(error) = started {
                    ui.show_error(error);
                }
                effect = Effect::Redraw;
            }
            if let Some(event) = ui.take_device_event() {
                if let Err(error) = setup.event(event, &mut screen, &mut ui) {
                    ui.show_error(error);
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
                    if catalog_tailnet && let Err(error) = screen.tailnet.ensure() {
                        ui.show_error(error);
                    }
                    let session = screen.tailnet.session();
                    catalog.set_transport_factory(std::sync::Arc::new(move |origins| {
                        let refs: Vec<_> = origins.iter().map(String::as_str).collect();
                        Ok(Box::new(femto_catalog::routing::Routed::new(
                            network::Transport,
                            tailnet_http::Transport(session.clone()),
                            &refs,
                        )?))
                    }));
                    log::info!("Catalog event: {event:?}");
                    if let Err(error) = catalog.event(event, &mut screen.catalog, &mut ui) {
                        ui.show_error(error);
                    }
                    effect = Effect::Redraw;
                }
            }
            if let (Some(event), Some(sync)) = (ui.take_kosync_event(), kosync.as_mut()) {
                if sync_tailnet && let Err(error) = screen.tailnet.ensure() {
                    ui.show_error(error);
                }
                if let Err(error) = sync.event(event, &reader, &mut ui, sync_transport!()) {
                    ui.show_error(error);
                }
                effect = Effect::Redraw;
            }
            // Input that arrived during the last panel update is handled
            // before drawing again, so typing and repeated turns are not
            // held up by one refresh per event.
            // A light step from a reading gesture changes no pixels.
            if effect != Effect::Redraw && ui.take_light_request() {
                let light = reader.state.frontlight;
                log::info!(
                    "Light: brightness {} warmth {}",
                    light.brightness,
                    light.warmth
                );
                if let Err(error) = screen.set_light(light.brightness, light.warmth) {
                    log::warn!("{error}");
                    reader.state.frontlight = previous_light;
                }
            }
            // Work this input started needs the radio awake for its first
            // packet; the drawing below would put that off by a second, and
            // a name lookup sent to a dozing radio has failed.
            attend!();
            dirty |= effect == Effect::Redraw;
            if dirty {
                carry = screen.event();
            }
            if dirty && carry.is_none() {
                dirty = false;
                if ui.take_light_request() {
                    let light = reader.state.frontlight;
                    if let Err(error) = screen.set_light(light.brightness, light.warmth) {
                        log::warn!("{error}");
                        reader.state.frontlight = previous_light;
                        ui.show_error(error);
                    }
                }
                let started = Instant::now();
                let handled = started - received;
                render(&mut reader, &ui, &mut plane)?;
                let rendered = started.elapsed();
                screen.present(&plane)?;
                let presented = started.elapsed();
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
                log::info!(
                    "Page {}/{}: input {} ms, render {} ms, display {} ms, save {} ms",
                    reader.page_index() + 1,
                    reader.page_count(),
                    handled.as_millis(),
                    rendered.as_millis(),
                    (presented - rendered).as_millis(),
                    (started.elapsed() - presented).as_millis()
                );
            }
        }
        if let Some(event) = event {
            match event {
                input::Event::Turn(turn) | input::Event::Swipe(turn) => {
                    turning_forward = turn == Turn::Next;
                }
                input::Event::Button(turn) => {
                    turning_forward = (turn == Turn::Previous) != reader.state.swap_page_buttons;
                }
                _ => {}
            }
            preparing = true;
        } else if preparing && !ui.is_open() && !ui.is_busy() {
            // Prepare the likely next page between inputs, then the opposite one.
            let levels = if cfg!(feature = "grayscale") { 4 } else { 2 };
            // Then index the rest of the chapter and the one after it.
            let prepared = reader
                .prepare_turn(turning_forward, levels)
                .and_then(|ran| Ok(ran || reader.prepare_turn(!turning_forward, levels)?))
                .and_then(|ran| Ok(ran || reader.index_step()?));
            // Nothing changes until the next input once both pages are ready
            // or preparation fails.
            match prepared {
                Ok(ran) => preparing = ran,
                Err(error) => {
                    log::warn!("Page preparation failed: {error}");
                    preparing = false;
                }
            }
        }
        if catalog.as_mut().is_some_and(|catalog| {
            let items = ui.menu_items(&reader);
            catalog.poll(&mut screen.catalog, &mut ui, items)
        }) {
            log::info!("Catalog view updated");
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
            if let Some(outcome) = sync.take_outcome() {
                log::info!("Sync: {outcome}");
            }
        }
        if ui.poll_scan() == Effect::Redraw {
            render(&mut reader, &ui, &mut plane)?;
            screen.present(&plane)?;
        }
        if setup.poll(&mut screen, &mut ui) {
            render(&mut reader, &ui, &mut plane)?;
            screen.present(&plane)?;
        }
        let navigated = ui.poll_navigation(&mut reader) == Effect::Redraw;
        // A new chapter has new neighbouring pages to prepare.
        preparing |= navigated;
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
        // The gray pass waits until input pauses, so rapid turns and menu
        // moves show the readable black-and-white frame without delay.
        if event.is_some() {
            last_input = Instant::now();
        } else if screen.is_unsettled()
            && !ui.is_busy()
            && last_input.elapsed() >= Duration::from_millis(350)
        {
            screen.settle(&plane)?;
        }
        // Pending work leaves the view in place. Mark it once the wait is
        // long enough to notice; the finished view replaces the mark.
        if !ui.is_busy() {
            busy_since = None;
        } else if let Some((since, shown)) = &mut busy_since {
            if !*shown && since.elapsed() >= Duration::from_millis(700) {
                *shown = true;
                render(&mut reader, &ui, &mut plane)?;
                screen.present_quick(&plane)?;
            }
        } else {
            busy_since = Some((Instant::now(), false));
        }
        let online = screen.wifi().is_some_and(|wifi| wifi.connected().is_some()) && clock::ready();
        if reader.state.sync_automatically
            && online
            && retiring.is_none()
            && !ui.is_opening()
            && reconciled.as_deref() != Some(reader.document.metadata.id.as_str())
            && let Some(sync) = kosync.as_mut()
            && !sync.is_busy()
        {
            reconciled = Some(reader.document.metadata.id.clone());
            if !sync_tailnet || screen.tailnet.ensure().is_ok() {
                let started = sync.reconcile(&reader, sync_transport!(), false);
                log::info!("Sync on opening: started={started}");
            }
        }
        if reader.state.check_updates && !update_checked && online {
            update_checked = true;
            if let Err(error) = updater.check(true) {
                log::warn!("Update check not started: {error}");
            }
            ui.set_update(Some(updater.view()));
        }
        let (update_changed, update_error) = updater.poll();
        // A download redraws its progress every few seconds at most.
        if updater.is_busy() && update_drawn.elapsed() >= Duration::from_secs(3) {
            update_drawn = Instant::now();
            if ui.set_update(Some(updater.view())) && ui.is_update_open() {
                render(&mut reader, &ui, &mut plane)?;
                screen.present(&plane)?;
            }
        }
        if update_changed {
            ui.set_update(Some(updater.view()));
            if let Some(error) = update_error {
                ui.open();
                ui.show_error(error);
            }
            if ui.is_open() {
                render(&mut reader, &ui, &mut plane)?;
                screen.present(&plane)?;
            }
        }
        let install = std::mem::take(&mut update_now)
            || ui.take_update_request()
                && match updater.activate() {
                    Ok(update::Activation::Install) => true,
                    Ok(update::Activation::Started) => {
                        ui.set_update(Some(updater.view()));
                        render(&mut reader, &ui, &mut plane)?;
                        screen.present(&plane)?;
                        false
                    }
                    Err(error) => {
                        ui.show_error(error);
                        render(&mut reader, &ui, &mut plane)?;
                        screen.present(&plane)?;
                        false
                    }
                };
        if install {
            // Recovery restarts into the new application, which resumes here.
            if let Some(store) = &mut journal {
                reader.record_position(
                    reader
                        .state
                        .books
                        .values()
                        .map(|book| book.last_opened)
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1),
                );
                if let Err(error) = store.save(&reader.state) {
                    log::warn!("State save before update failed: {error}");
                }
            }
            let levels = if cfg!(feature = "grayscale") { 4 } else { 2 };
            let lines = [
                String::new(),
                "Installing the update.".into(),
                "Keep the reader charged or plugged in.".into(),
                "When it says Update complete,".into(),
                "tap Restart.".into(),
            ];
            let pixels = reader
                .render_panel(&lines, levels)
                .map_err(|e| e.to_string())?;
            pack_pixels(&pixels, reader.state.rotation, &mut plane)?;
            screen.present_clean(&plane)?;
            let error = update::install();
            log::warn!("Update not started: {error}");
            updater.rescan();
            ui.set_update(Some(updater.view()));
            ui.open();
            ui.show_error(error);
            render(&mut reader, &ui, &mut plane)?;
            screen.present(&plane)?;
        }
        attend!();
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
                || updater.is_busy()
                || kosync.as_ref().is_some_and(|sync| sync.is_busy()),
        );
        // Deep sleep drops the USB connection, so an attached host keeps the
        // device awake; it is then powered and usually being driven over USB.
        let hosted = unsafe { esp_idf_svc::sys::usb_serial_jtag_is_connected() };
        let wake_after = timed_sleep.take();
        let asked =
            manual_sleep || wake_after.is_some() || (automatic_sleep && !stay_awake && !hosted);
        // When to sleep now, and the checksum of a sleep page already shown.
        let mut sleeping = None::<(Option<Duration>, Option<u32>)>;
        if let Some((wake_after, shown)) = retiring {
            if !kosync.as_ref().is_some_and(|sync| sync.is_busy()) {
                log::info!("Sync before sleep: finished");
                retiring = None;
                sleeping = Some((wake_after, Some(shown)));
            }
        } else if asked {
            idle.suppress_until_activity();
            // The position goes to the sync server first, behind the sleep
            // page, so that sleeping looks immediate.
            let unsent = reader.state.sync_automatically
                && online
                && journal.is_some()
                && kosync
                    .as_ref()
                    .is_some_and(|sync| !sync.is_busy() && sync.has_unsent(&reader));
            let shown = unsent
                .then(|| -> Result<u32, String> {
                    power::wait_for_release()?;
                    ui.release_dictionary();
                    let levels = if cfg!(feature = "grayscale") { 4 } else { 2 };
                    let pixels = ui
                        .render_sleep(&mut reader, levels)
                        .map_err(|e| format!("Sleep screen failed: {e}"))?;
                    pack_pixels(&pixels, reader.state.rotation, &mut plane)?;
                    screen.present_clean(&plane)?;
                    Ok(femto_picco_board::console::crc32(0, &plane))
                })
                .and_then(Result::ok)
                .filter(|_| {
                    (!sync_tailnet || screen.tailnet.ensure().is_ok())
                        && kosync
                            .as_mut()
                            .is_some_and(|sync| sync.reconcile(&reader, sync_transport!(), true))
                });
            match shown {
                Some(shown) => {
                    log::info!("Sync before sleep: started");
                    retiring = Some((wake_after, shown));
                }
                None => sleeping = Some((wake_after, None)),
            }
        }
        if let Some((wake_after, shown)) = sleeping {
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
                let levels = if cfg!(feature = "grayscale") { 4 } else { 2 };
                let pixels = ui
                    .render_sleep(&mut reader, levels)
                    .map_err(|e| format!("Sleep screen failed: {e}"))?;
                pack_pixels(&pixels, reader.state.rotation, &mut plane)?;
                // Unless it is what the panel already shows.
                if shown != Some(femto_picco_board::console::crc32(0, &plane)) {
                    screen.present_clean(&plane)?;
                }
                screen.prepare_sleep()
            })();
            match prepared {
                Ok(()) => {
                    drop(ui);
                    drop(reader);
                    drop(journal);
                    drop(screen);
                    power::enter(wake_after);
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
            report_heap("running");
            log::info!("battery: {:?}", screen.battery());
            heap_report = Instant::now();
        }
    }
}

const FONTS: &str = "/sdcard/femto/fonts";

/// The chosen card family, or the card's own `regular.ttf` family, over the
/// embedded Libron, with the card's fallback faces.
fn load_fonts(card: bool, family: Option<&str>) -> Result<Fonts, String> {
    // Libron, embedded unmodified under the SIL Open Font License; see fonts/.
    let default_fonts = || -> femto_reader::Result<Fonts> {
        let face = |bytes: &[u8]| Font::from_bytes(bytes.to_vec());
        let mut fonts = Fonts::new(face(include_bytes!("../fonts/Libron-Regular.ttf"))?);
        fonts.bold = Some(face(include_bytes!("../fonts/Libron-Bold.ttf"))?);
        fonts.italic = Some(face(include_bytes!("../fonts/Libron-Italic.ttf"))?);
        fonts.bold_italic = Some(face(include_bytes!("../fonts/Libron-BoldItalic.ttf"))?);
        Ok(fonts)
    };
    if !card {
        return default_fonts().map_err(|e| e.to_string());
    }
    let loaded = femto_reader::fontset::load(
        std::path::Path::new(FONTS),
        family,
        default_fonts,
        4 * 1024 * 1024,
    )
    .map_err(|e| e.to_string())?;
    for warning in &loaded.warnings {
        log::warn!("Font: {warning}");
    }
    log::info!("Loaded {} bytes of SD font data", loaded.bytes);
    Ok(loaded.fonts)
}

fn startup_reader(
    screen: &mut panel::Screen,
    fonts: Fonts,
    state: ReaderState,
    candidates: Vec<(std::path::PathBuf, bool)>,
    index_store: Option<femto_reader::page_index::IndexStore>,
) -> Result<(Reader, bool), String> {
    let specimen = || {
        Document::from_text(
            "Femto reader specimen",
            include_str!("../../../tests/fixtures/device-reader.txt"),
        )
    };
    let rotation = state.rotation;
    let mut startup =
        ReaderStartup::from_document(specimen().map_err(|e| e.to_string())?, fonts, state)
            .map_err(|e| e.to_string())?;
    startup.set_index_store(index_store.clone());
    let levels = if cfg!(feature = "grayscale") { 4 } else { 2 };
    let mut plane =
        vec![255; femto_picco_board::PLANE_BYTES * if cfg!(feature = "grayscale") { 2 } else { 1 }];
    'books: for (path, resumed) in candidates {
        if let Err(error) = std::fs::write(OPENING_MARKER, path.as_os_str().as_encoded_bytes()) {
            log::warn!("Cannot mark {} as opening: {error}", path.display());
        }
        if let Err(error) = startup.open_cached_file(&path, "/sdcard/femto", 256) {
            log::warn!("Cannot open {}: {error}", path.display());
            continue;
        }
        let pixels = startup.loading_frame(levels).map_err(|e| e.to_string())?;
        pack_pixels(&pixels, rotation, &mut plane)?;
        drop(pixels);
        screen.present_oriented(&plane, orientation(rotation))?;
        loop {
            std::thread::sleep(Duration::from_millis(10));
            screen.poll_wifi();
            if screen.event().is_some() {
                log::info!("Startup opening cancelled");
                break 'books;
            }
            let stepped = Instant::now();
            let step = startup.step_for(Duration::from_millis(5));
            if stepped.elapsed() >= Duration::from_millis(250) {
                log::info!(
                    "Startup step took {} ms; {} pages indexed",
                    stepped.elapsed().as_millis(),
                    startup.indexed_pages()
                );
                report_heap("startup");
            }
            match step {
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
                .map(|mut reader| {
                    reader.set_index_store(index_store);
                    (reader, false)
                })
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
