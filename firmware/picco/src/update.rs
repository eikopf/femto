//! Firmware updates from the SD card.
//!
//! The stock recovery application in the factory slot installs
//! `/sdcard/update/firmware.bin` into the single application slot when it
//! finds the pending marker beside it. It validates the image, keeps the
//! marker until installation succeeds and then boots the application, so an
//! interrupted installation resumes at the next start. Femto only stages the
//! package and restarts into recovery.
//!
//! Packages also come from the project's GitHub releases. A release carries
//! the image and a manifest with its version, length and SHA-256; the reader
//! fetches the manifest of the latest release, and downloads the image to
//! the card when its version is newer. Installing is a separate step.
use esp_idf_svc::sys;
use femto_catalog::http;
use femto_reader::ui::{UpdateStep, UpdateView};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const RELEASE: &str = "https://github.com/eikopf/femto/releases/latest/download/";
const PARTIAL: &str = "/sdcard/update/firmware.bin.part";
/// The application slot of the stock partition table.
const SLOT_BYTES: u64 = 0xE7_0000;

/// Where a USB push leaves a package; the console writes below the data directory.
const PUSHED: &str = "/sdcard/femto/firmware.bin";
const PACKAGE: &str = "/sdcard/update/firmware.bin";
const MARKER: &str = "/sdcard/update/.ota_pending";
/// Image and segment headers precede the application description.
const DESCRIPTION: usize = 32;
const VERSION_PREFIX: &str = "femto-";

fn text(field: &[u8]) -> &str {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    core::str::from_utf8(&field[..end]).unwrap_or("")
}

/// The version of a femto application image, read from its description.
fn version(path: &Path) -> Option<String> {
    let mut head = [0u8; DESCRIPTION + 48];
    fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    let description = &head[DESCRIPTION..];
    let valid = head[0] == sys::ESP_IMAGE_HEADER_MAGIC as u8
        && description[..4] == sys::ESP_APP_DESC_MAGIC_WORD.to_le_bytes();
    let version = text(&description[16..]);
    (valid && version.starts_with(VERSION_PREFIX)).then(|| version.into())
}

/// The version of the running application.
pub fn running() -> String {
    let description = unsafe { &*sys::esp_app_get_description() };
    let bytes = description.version.map(|c| c as u8);
    text(&bytes).into()
}

/// The version of a package waiting on the card.
pub fn waiting() -> Option<String> {
    [PUSHED, PACKAGE]
        .iter()
        .find_map(|path| version(Path::new(path)))
}

/// Hands the waiting package to recovery and restarts into it.
/// Returns only when that could not be arranged; the application is untouched.
pub fn install() -> String {
    let staged = (|| -> Result<(), String> {
        let package = Path::new(PACKAGE);
        if version(Path::new(PUSHED)).is_some() {
            fs::create_dir_all(package.parent().unwrap_or(package)).map_err(|e| e.to_string())?;
            // FAT rename does not replace an existing file.
            let _ = fs::remove_file(package);
            fs::rename(PUSHED, package).map_err(|e| e.to_string())?;
        }
        version(package).ok_or("No femto update on the card")?;
        fs::File::create(MARKER)
            .and_then(|marker| marker.sync_all())
            .map_err(|e| e.to_string())?;
        let recovery = unsafe {
            sys::esp_partition_find_first(
                sys::esp_partition_type_t_ESP_PARTITION_TYPE_APP,
                sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_APP_FACTORY,
                core::ptr::null(),
            )
        };
        if recovery.is_null() {
            return Err("No recovery application on this device".into());
        }
        // Verifies the recovery image before selecting it.
        sys::esp!(unsafe { sys::esp_ota_set_boot_partition(recovery) }).map_err(|e| e.to_string())
    })();
    match staged {
        Ok(()) => unsafe { sys::esp_restart() },
        Err(error) => {
            let _ = fs::remove_file(MARKER);
            error
        }
    }
}

/// The numbers of a `femto-X.Y.Z` version, for ordering.
fn numbers(version: &str) -> Option<[u32; 3]> {
    let mut parts = version.strip_prefix(VERSION_PREFIX)?.split('.');
    let numbers = [(); 3].map(|()| parts.next().and_then(|part| part.parse().ok()));
    match (numbers, parts.next()) {
        ([Some(major), Some(minor), Some(patch)], None) => Some([major, minor, patch]),
        _ => None,
    }
}

fn short(version: &str) -> &str {
    version.strip_prefix(VERSION_PREFIX).unwrap_or(version)
}

/// A release's manifest, as published beside its image.
#[derive(Clone, serde::Deserialize)]
struct Manifest {
    version: String,
    bytes: u64,
    sha256: String,
}

fn fetch(
    name: &str,
    limit: usize,
    offset: u64,
    seconds: u64,
    output: &mut impl Write,
) -> Result<(), http::Error> {
    http::download_file(
        &mut crate::network::Transport,
        &format!("{RELEASE}{name}"),
        Instant::now() + Duration::from_secs(seconds),
        Arc::new(AtomicBool::new(false)),
        limit,
        offset,
        output,
    )
    .map(|_| ())
}

fn check() -> Result<Manifest, String> {
    let mut bytes = Vec::new();
    fetch("femto-picco.json", 4096, 0, 60, &mut bytes).map_err(|e| e.to_string())?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| "Release manifest is not valid")?;
    if numbers(&manifest.version).is_none()
        || manifest.bytes > SLOT_BYTES
        || manifest.sha256.len() != 64
    {
        return Err("Release manifest is not valid".into());
    }
    Ok(manifest)
}

/// Attempts that receive nothing before a download gives up.
const STALLED_ATTEMPTS: u32 = 4;

/// Counts the bytes that reach the card, for the progress bar.
struct Counted<'a>(fs::File, &'a AtomicUsize);
impl Write for Counted<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.0.write(bytes)?;
        self.1.fetch_add(written, Ordering::Relaxed);
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// Downloads the release image and publishes it as the waiting package only
/// when it is the image the manifest describes. A broken connection resumes
/// from the bytes already on the card; a download that gives up leaves them
/// for the next try, and one that fails verification is deleted. `received`
/// follows the bytes on the card.
fn download(manifest: &Manifest, received: &AtomicUsize) -> Result<(), String> {
    let partial = Path::new(PARTIAL);
    fs::create_dir_all(partial.parent().unwrap_or(partial)).map_err(|e| e.to_string())?;
    let held = || fs::metadata(partial).map_or(0, |metadata| metadata.len());
    let mut stalled = 0;
    while held() < manifest.bytes {
        let offset = held();
        received.store(offset as usize, Ordering::Relaxed);
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(partial)
            .map_err(|e| e.to_string())?;
        let mut file = Counted(file, received);
        let result = fetch(
            "femto-picco.bin",
            (manifest.bytes - offset) as usize,
            offset,
            600,
            &mut file,
        );
        file.0.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        let Err(error) = result else { break };
        log::warn!("Update download stopped at {} bytes: {error}", held());
        if matches!(error, http::Error::Status(200)) {
            // The server sent the whole image; start again without a range.
            fs::remove_file(partial).map_err(|e| e.to_string())?;
        } else if held() > offset {
            stalled = 0;
        } else {
            stalled += 1;
            if stalled == STALLED_ATTEMPTS {
                return Err(error.to_string());
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    received.store(manifest.bytes as usize, Ordering::Relaxed);
    let verified = (|| -> Result<(), String> {
        let digest =
            femto_reader::hashing::digest(fs::File::open(partial).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let digest: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        if held() != manifest.bytes
            || !digest.eq_ignore_ascii_case(&manifest.sha256)
            || version(partial).as_deref() != Some(manifest.version.as_str())
        {
            return Err("Downloaded update does not match its release".into());
        }
        // FAT rename does not replace an existing file.
        let _ = fs::remove_file(PACKAGE);
        fs::rename(partial, PACKAGE).map_err(|e| e.to_string())
    })();
    if verified.is_err() {
        let _ = fs::remove_file(partial);
    }
    verified
}

enum Step {
    Idle,
    Checking,
    /// The latest release is not newer than the running application.
    Current,
    Available(Manifest),
    /// With the bytes on the card so far.
    Downloading(Manifest, Arc<AtomicUsize>),
}

enum Job {
    Check(JoinHandle<Result<Manifest, String>>),
    Download(JoinHandle<Result<(), String>>),
}

/// What activating the update row asks of the caller.
pub enum Activation {
    Install,
    Started,
}

/// Finds, downloads and offers firmware updates; one network job at a time.
pub struct Updater {
    step: Step,
    job: Option<Job>,
    /// The version of the package on the card.
    waiting: Option<String>,
    /// The job was not started by the user, so its failure is not shown.
    quiet: bool,
}

impl Updater {
    pub fn new() -> Self {
        Self {
            step: Step::Idle,
            job: None,
            waiting: waiting(),
            quiet: false,
        }
    }

    /// Looks at the card again, after a file arrived or an installation failed.
    pub fn rescan(&mut self) {
        self.waiting = waiting();
    }

    pub fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    /// The firmware update panel.
    pub fn view(&self) -> UpdateView {
        let megabytes = |bytes: u64| format!("{:.1}", bytes as f64 / 1e6);
        let installed = || format!("Version {} is installed", short(&running()));
        let (status, step) = match (&self.waiting, &self.step) {
            (_, Step::Checking) => (
                installed(),
                UpdateStep::Working {
                    label: "Checking for updates".into(),
                    percent: None,
                },
            ),
            (_, Step::Downloading(release, received)) => {
                let received = (received.load(Ordering::Relaxed) as u64).min(release.bytes);
                let label = if received == release.bytes {
                    "Verifying the download".into()
                } else {
                    format!(
                        "Downloaded {} of {} MB",
                        megabytes(received),
                        megabytes(release.bytes)
                    )
                };
                (
                    format!("Version {} is available", short(&release.version)),
                    UpdateStep::Working {
                        label,
                        percent: Some((received * 100 / release.bytes.max(1)) as u8),
                    },
                )
            }
            (Some(version), _) => (
                format!("Version {} is ready to install", short(version)),
                UpdateStep::Action("Install and restart".into()),
            ),
            (None, Step::Available(release)) => (
                format!("Version {} is available", short(&release.version)),
                UpdateStep::Action(format!("Download {} MB", megabytes(release.bytes))),
            ),
            (None, Step::Current) => (
                format!("Version {} is the latest", short(&running())),
                UpdateStep::Action("Check again".into()),
            ),
            (None, Step::Idle) => (installed(), UpdateStep::Action("Check for updates".into())),
        };
        UpdateView { status, step }
    }

    /// A short note for the status row when an update can be had.
    pub fn notice(&self) -> Option<String> {
        match (&self.waiting, &self.step) {
            (Some(version), _) => Some(format!("Update {} ready", short(version))),
            (None, Step::Available(release)) => Some(format!("Update {}", short(&release.version))),
            (None, Step::Downloading(..)) => Some("Downloading update".into()),
            _ => None,
        }
    }

    fn spawn<T: Send + 'static>(
        operation: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<JoinHandle<Result<T, String>>, String> {
        std::thread::Builder::new()
            .name("femto-update".into())
            .stack_size(64 * 1024)
            .spawn(operation)
            .map_err(|e| e.to_string())
    }

    /// Starts looking for a newer release. `quiet` suppresses its failure.
    pub fn check(&mut self, quiet: bool) -> Result<(), String> {
        if self.job.is_none() {
            self.job = Some(Job::Check(Self::spawn(check)?));
            self.step = Step::Checking;
            self.quiet = quiet;
        }
        Ok(())
    }

    /// Performs the update row's current action.
    pub fn activate(&mut self) -> Result<Activation, String> {
        if self.job.is_some() {
            return Ok(Activation::Started);
        }
        if self.waiting.is_some() {
            return Ok(Activation::Install);
        }
        if let Step::Available(release) = &self.step {
            let release = release.clone();
            let task = release.clone();
            let received = Arc::new(AtomicUsize::new(0));
            let counter = received.clone();
            self.job = Some(Job::Download(Self::spawn(move || {
                download(&task, &counter)
            })?));
            self.step = Step::Downloading(release, received);
            self.quiet = false;
        } else {
            self.check(false)?;
        }
        Ok(Activation::Started)
    }

    /// Collects a finished job. Returns whether the panel changed and the
    /// failure to show, if any.
    pub fn poll(&mut self) -> (bool, Option<String>) {
        if !self.job.as_ref().is_some_and(|job| match job {
            Job::Check(task) => task.is_finished(),
            Job::Download(task) => task.is_finished(),
        }) {
            return (false, None);
        }
        fn flatten<T>(joined: std::thread::Result<Result<T, String>>) -> Result<T, String> {
            joined.unwrap_or_else(|_| Err("Update task failed".into()))
        }
        let result = match self.job.take() {
            Some(Job::Check(task)) => flatten(task.join()).map(|release| {
                let newer = numbers(&release.version) > numbers(&running());
                log::info!("Update check: latest {} newer={newer}", release.version);
                self.step = if newer {
                    Step::Available(release)
                } else {
                    Step::Current
                };
            }),
            Some(Job::Download(task)) => flatten(task.join()).map(|()| self.step = Step::Idle),
            None => Ok(()),
        };
        self.rescan();
        let error = result.err().and_then(|error| {
            log::warn!("Update: {error}");
            // A failed download can be tried again without another check.
            self.step = match std::mem::replace(&mut self.step, Step::Idle) {
                Step::Downloading(release, _) => Step::Available(release),
                _ => Step::Idle,
            };
            (!self.quiet).then_some(error)
        });
        (true, error)
    }
}
