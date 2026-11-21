// Compile the device menu controller on the host with an injected transport.
#[path = "../../../firmware/picco/src/catalog.rs"]
mod catalog;
use catalog::test_network as network;

#[test]
fn sd_credentials_load_before_any_request_and_errors_disable_catalog() {
    use femto_catalog::worker::Worker;
    use femto_reader::ui::{CatalogEvent, ReaderUi};
    use std::{
        fs,
        time::{Duration, Instant},
    };
    let root = tempfile::tempdir().unwrap();
    assert!(
        catalog::CatalogMenu::load_from(root.path())
            .unwrap()
            .is_none()
    );
    fs::write(
        root.path().join("catalog.url"),
        "https://auth.example/start\n",
    )
    .unwrap();
    let auth_path = root.path().join("catalog-auth.json");
    fs::write(&auth_path, br#"{"username":"reader","password":secret}"#).unwrap();
    let error = catalog::CatalogMenu::load_from(root.path()).err().unwrap();
    assert!(!error.contains("reader") && !error.contains("secret"));
    fs::write(&auth_path, vec![b' '; 8193]).unwrap();
    assert!(catalog::CatalogMenu::load_from(root.path()).is_err());
    fs::remove_file(&auth_path).unwrap();
    fs::create_dir(&auth_path).unwrap();
    assert!(catalog::CatalogMenu::load_from(root.path()).is_err());
    fs::remove_dir(&auth_path).unwrap();
    fs::write(&auth_path, br#"{"username":"reader","password":"secret"}"#).unwrap();
    let mut menu = catalog::CatalogMenu::load_from(root.path())
        .unwrap()
        .unwrap();
    let mut worker = Worker::default();
    let mut ui = ReaderUi::default();
    menu.event(CatalogEvent::Open, &mut worker, &mut ui)
        .unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    while worker.is_busy() {
        menu.poll(&mut worker, &mut ui, 6);
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(ui.is_catalog_open());
}

#[test]
fn download_publishes_valid_epub_and_starts_library_rescan() {
    use femto_catalog::worker::Worker;
    use femto_reader::ui::{CatalogEvent, ReaderUi};
    use std::{
        fs,
        time::{Duration, Instant},
    };
    fn finish(menu: &mut catalog::CatalogMenu, jobs: &mut catalog::Jobs, ui: &mut ReaderUi) {
        let until = Instant::now() + Duration::from_secs(5);
        while jobs.is_busy() {
            menu.poll(jobs, ui, 6);
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("catalog.url"),
        "https://auth.example/start",
    )
    .unwrap();
    fs::write(
        root.path().join("catalog-auth.json"),
        br#"{"username":"reader","password":"secret"}"#,
    )
    .unwrap();
    let mut menu = catalog::CatalogMenu::load_from(root.path())
        .unwrap()
        .unwrap();
    let mut jobs = Worker::default();
    let mut ui = ReaderUi::default();
    ui.configure_library_scan(root.path().to_owned(), 256)
        .unwrap();
    menu.event(CatalogEvent::Open, &mut jobs, &mut ui).unwrap();
    finish(&mut menu, &mut jobs, &mut ui);
    menu.event(CatalogEvent::Activate(0), &mut jobs, &mut ui)
        .unwrap();
    menu.event(CatalogEvent::Activate(2), &mut jobs, &mut ui)
        .unwrap();
    finish(&mut menu, &mut jobs, &mut ui);
    assert!(ui.is_scanning());
    while ui.is_scanning() {
        ui.poll_scan();
    }
    let library = femto_reader::state::Library::scan(root.path()).unwrap();
    assert_eq!(library.entries.len(), 1);
    let path = &library.entries[0].path;
    assert_eq!(
        fs::read(path).unwrap(),
        include_bytes!("../../../tests/fixtures/sample.epub")
    );
    let names: std::collections::BTreeSet<_> = fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        names,
        [
            std::ffi::OsString::from("catalog.url"),
            std::ffi::OsString::from("catalog-auth.json"),
            std::ffi::OsString::from(".femto-library-cache.json"),
            path.file_name().unwrap().to_owned(),
        ]
        .into_iter()
        .collect()
    );
    // A repeated acquisition must leave the existing book untouched.
    menu.event(CatalogEvent::Open, &mut jobs, &mut ui).unwrap();
    menu.event(CatalogEvent::Activate(0), &mut jobs, &mut ui)
        .unwrap();
    menu.event(CatalogEvent::Activate(2), &mut jobs, &mut ui)
        .unwrap();
    finish(&mut menu, &mut jobs, &mut ui);
    assert!(!ui.is_scanning());
    assert_eq!(
        fs::read(path).unwrap(),
        include_bytes!("../../../tests/fixtures/sample.epub")
    );
    assert_eq!(
        fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>(),
        names
    );
}

#[test]
fn invalid_and_cancelled_downloads_leave_no_published_or_partial_book() {
    use femto_catalog::worker::Worker;
    use femto_reader::ui::{CatalogEvent, ReaderUi};
    use std::{
        fs,
        time::{Duration, Instant},
    };
    for (source, cancel) in [
        ("https://example.com/badfeed", false),
        ("https://example.com/start", true),
    ] {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("catalog.url"), source).unwrap();
        let mut menu = catalog::CatalogMenu::load_from(root.path())
            .unwrap()
            .unwrap();
        let mut jobs = Worker::default();
        let mut ui = ReaderUi::default();
        ui.configure_library_scan(root.path().to_owned(), 256)
            .unwrap();
        menu.event(CatalogEvent::Open, &mut jobs, &mut ui).unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while jobs.is_busy() {
            menu.poll(&mut jobs, &mut ui, 6);
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(1));
        }
        menu.event(CatalogEvent::Activate(0), &mut jobs, &mut ui)
            .unwrap();
        menu.event(CatalogEvent::Activate(2), &mut jobs, &mut ui)
            .unwrap();
        if cancel {
            menu.event(CatalogEvent::Back, &mut jobs, &mut ui).unwrap();
        }
        while jobs.is_busy() {
            menu.poll(&mut jobs, &mut ui, 6);
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(ui.is_catalog_open());
        assert!(!ui.is_scanning());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}

#[test]
fn configured_tailnet_origins_require_a_factory_and_reach_each_request() {
    use femto_reader::ui::{CatalogEvent, ReaderUi};
    use std::{
        fs,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("catalog.url"), "http://books.example/feed").unwrap();
    let route = root.path().join("catalog-tailnet-origins");
    for bad in [
        "http://user:secret@books.example",
        "http://books.example/path",
        "ftp://books.example",
        &"http://books.example\n".repeat(17),
        &"x".repeat(8193),
    ] {
        fs::write(&route, bad).unwrap();
        assert!(catalog::CatalogMenu::load_from(root.path()).is_err());
    }
    fs::write(&route, "http://books.example\n").unwrap();
    let mut menu = catalog::CatalogMenu::load_from(root.path())
        .unwrap()
        .unwrap();
    let mut jobs = catalog::Jobs::default();
    let mut ui = ReaderUi::default();
    let mut unavailable = catalog::CatalogMenu::load_from(root.path())
        .unwrap()
        .unwrap();
    unavailable
        .event(CatalogEvent::Open, &mut jobs, &mut ui)
        .unwrap();
    let end = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(result) = jobs.poll() {
            assert!(
                matches!(result.result, Err(femto_catalog::http::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotConnected)
            );
            break;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(1));
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    menu.set_transport_factory(Arc::new(move |origins| {
        assert_eq!(origins, &["http://books.example"]);
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(network::Transport))
    }));
    menu.event(CatalogEvent::Open, &mut jobs, &mut ui).unwrap();
    let end = Instant::now() + Duration::from_secs(2);
    while jobs.is_busy() {
        menu.poll(&mut jobs, &mut ui, 6);
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
