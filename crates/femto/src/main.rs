#[cfg(all(unix, not(target_os = "espidf")))]
mod host_trust;
use std::{
    fs::{self, File},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use clap::{Parser, Subcommand};
use femto_reader::{
    controller::{Action, Reader},
    display::TextRaster,
    document::Document,
    layout::{Font, Fonts, LayoutConfig, paginate, rasterize_display_report, write_png},
    state::{Library, ReaderState},
};

#[cfg(all(unix, not(target_os = "espidf")))]
mod catalog;
mod clippings;
mod progress;
#[cfg(all(unix, not(target_os = "espidf")))]
mod progress_service;
#[cfg(all(unix, not(target_os = "espidf")))]
mod progress_sync;
#[cfg(all(unix, not(target_os = "espidf")))]
mod state_file;
#[cfg(all(unix, not(target_os = "espidf")))]
mod tailnet;

#[derive(Parser)]
#[command(version, about = "Femto e-reader host tools")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct RasterArgs {
    /// Extra outline weight in physical pixels (0 to 0.75).
    #[arg(long)]
    text_stroke: Option<f32>,
    /// Ink coverage exponent (0.3 to 3); below one darkens edges.
    #[arg(long)]
    text_gamma: Option<f32>,
    #[arg(long, action = clap::ArgAction::Set)]
    text_hinting: Option<bool>,
    #[arg(long, action = clap::ArgAction::Set)]
    fractional_text: Option<bool>,
}

impl RasterArgs {
    fn apply(self, mut profile: TextRaster) -> femto_reader::Result<TextRaster> {
        if let Some(value) = self.text_stroke {
            profile.stroke = value;
        }
        if let Some(value) = self.text_gamma {
            profile.coverage_gamma = value;
        }
        if let Some(value) = self.text_hinting {
            profile.hinting = value;
        }
        if let Some(value) = self.fractional_text {
            profile.fractional = value;
        }
        profile.validate()?;
        Ok(profile)
    }
}

#[derive(Subcommand)]
enum Command {
    /// Look up an offline StarDict word and print definitions as JSON.
    DictionaryLookup { ifo: PathBuf, word: String },
    /// Export source-verified saved excerpts as JSON on stdout.
    ClippingsExport {
        book: PathBuf,
        #[arg(long, required_unless_present = "journal", conflicts_with = "journal")]
        state: Option<PathBuf>,
        /// Read an offline copy of the Picco state-slot directory.
        #[arg(long, required_unless_present = "state", conflicts_with = "state")]
        journal: Option<PathBuf>,
    },
    /// Synchronize one book using a durable shared-position checkpoint.
    #[cfg(all(unix, not(target_os = "espidf")))]
    ProgressSync(progress_sync::SyncArgs),
    /// Serve a persistent progress store on a local loopback endpoint.
    #[cfg(all(unix, not(target_os = "espidf")))]
    ProgressServe(progress_service::ServeArgs),
    /// Upload saved progress with a required remote precondition.
    #[cfg(all(unix, not(target_os = "espidf")))]
    ProgressPush(progress::PushArgs),
    /// Fetch and apply a Femto progress record over HTTP or verified HTTPS.
    #[cfg(all(unix, not(target_os = "espidf")))]
    ProgressPull(progress::PullArgs),
    /// Export an exact-content reading position as bounded JSON.
    ProgressExport {
        book: PathBuf,
        #[arg(long)]
        state: PathBuf,
    },
    /// Import progress with optional optimistic comparison against a local snapshot.
    ProgressImport {
        book: PathBuf,
        record: PathBuf,
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        expected: Option<PathBuf>,
    },
    /// Browse OPDS navigation, facets, pagination, and publication metadata.
    #[cfg(all(unix, not(target_os = "espidf")))]
    OpdsBrowse(catalog::BrowseArgs),
    /// Acquire one direct EPUB from an OPDS feed into a new validated file.
    #[cfg(all(unix, not(target_os = "espidf")))]
    OpdsDownload(catalog::DownloadArgs),
    /// Fetch an OPDS 2.0 catalog through HTTP or certificate-verified HTTPS.
    #[cfg(all(unix, not(target_os = "espidf")))]
    OpdsFetch {
        source: String,
        /// Numeric address override for this origin; TLS still verifies the URL host.
        #[arg(long)]
        endpoint: Option<std::net::SocketAddr>,
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=60))]
        timeout: u64,
        /// Private JSON file containing username and password for this origin.
        #[arg(long)]
        credentials: Option<PathBuf>,
        /// Permit Basic credentials over plaintext HTTP for the configured origin.
        #[arg(long, requires = "credentials")]
        allow_http_auth: bool,
    },
    /// Inspect a saved OPDS 2.0 feed, resolving links against its original URL.
    OpdsInspect {
        feed: PathBuf,
        #[arg(long)]
        source: String,
    },
    /// Create or inspect a durable host Tailscale identity; prints public keys only.
    #[cfg(all(unix, not(target_os = "espidf")))]
    TailnetIdentity { directory: PathBuf },
    /// Register a persistent identity and optionally observe authenticated maps.
    #[cfg(all(unix, not(target_os = "espidf")))]
    TailnetConnect(Box<tailnet::ConnectArgs>),
    /// Read book metadata and contents.
    Inspect { book: PathBuf },
    /// Scan a local library; report malformed books separately.
    Library {
        root: PathBuf,
        #[arg(long, default_value = "")]
        search: String,
    },
    /// Render one chapter into PNG pages with the same core used by the reader.
    Render {
        #[command(flatten)]
        raster: RasterArgs,
        book: PathBuf,
        #[arg(long)]
        font: PathBuf,
        #[arg(long)]
        bold_font: Option<PathBuf>,
        #[arg(long)]
        italic_font: Option<PathBuf>,
        #[arg(long)]
        bold_italic_font: Option<PathBuf>,
        #[arg(long)]
        fallback_font: Vec<PathBuf>,
        #[arg(long, default_value_t = 0)]
        chapter: usize,
        #[arg(long, default_value_t = 24.0)]
        font_size: f32,
        #[arg(long, default_value_t = 480)]
        width: u32,
        #[arg(long, default_value_t = 800)]
        height: u32,
        #[arg(long, default_value_t = 24)]
        margin: u32,
        #[arg(long, default_value = "output")]
        output: PathBuf,
        #[arg(long, default_value_t = 256, value_parser = parse_levels)]
        levels: u16,
    },
    /// Turn pages interactively; writes the current display to a PNG file.
    Read {
        #[command(flatten)]
        raster: RasterArgs,
        book: PathBuf,
        #[arg(long)]
        font: PathBuf,
        #[arg(long)]
        fallback_font: Vec<PathBuf>,
        #[arg(long, default_value = "output/reader.json")]
        state: PathBuf,
        #[arg(long, default_value = "output/display.png")]
        display: PathBuf,
        #[arg(long, default_value_t = 256, value_parser = parse_levels)]
        levels: u16,
    },
}

fn main() -> Result<()> {
    match Args::parse().command {
        #[cfg(all(unix, not(target_os = "espidf")))]
        Command::ProgressSync(args) => progress_sync::sync(args)?,
        #[cfg(all(unix, not(target_os = "espidf")))]
        Command::ProgressServe(args) => progress_service::serve(args)?,
        #[cfg(all(unix, not(target_os = "espidf")))]
        Command::ProgressPush(args) => progress::push(args)?,
        #[cfg(all(unix, not(target_os = "espidf")))]
        Command::ProgressPull(args) => progress::pull(args)?,
        Command::DictionaryLookup { ifo, word } => {
            let mut dictionary = femto_reader::dictionary::Dictionary::open(ifo)?;
            while !dictionary.prepare_step()? {}
            let definitions = dictionary.lookup(&word)?;
            let definitions: Vec<_> = definitions
                .iter()
                .map(|entry| serde_json::json!({"word": entry.word, "text": entry.text}))
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"dictionary": dictionary.name(), "definitions": definitions})
                )?
            );
        }
        Command::ProgressExport { book, state } => progress::export(&book, &state)?,
        Command::ClippingsExport {
            book,
            state,
            journal,
        } => match (state, journal) {
            (Some(state), None) => clippings::export(&book, &state)?,
            (None, Some(journal)) => clippings::export_journal(&book, &journal)?,
            _ => anyhow::bail!("select one clipping state source"),
        },
        Command::ProgressImport {
            book,
            record,
            state,
            expected,
        } => progress::import(&book, &record, &state, expected.as_deref())?,
        #[cfg(all(unix, not(target_os = "espidf")))]
        Command::TailnetConnect(args) => tailnet::connect(*args)?,
        #[cfg(all(unix, not(target_os = "espidf")))]
        Command::TailnetIdentity { directory } => {
            let store = femto_microlink::store::HostStore::open(directory)?;
            let identity = store.identity()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "machine_key": identity.machine_key().public().to_string(),
                    "node_key": identity.node_key().public().to_string(),
                    "disco_key": identity.disco_key().public().to_string(),
                }))?
            );
        }
        #[cfg(all(unix, not(target_os = "espidf")))]
        Command::OpdsBrowse(args) => catalog::browse(args)?,
        #[cfg(all(unix, not(target_os = "espidf")))]
        Command::OpdsDownload(args) => catalog::download(args)?,
        #[cfg(all(unix, not(target_os = "espidf")))]
        Command::OpdsFetch {
            source,
            endpoint,
            timeout,
            credentials,
            allow_http_auth,
        } => {
            let auth = credentials
                .as_deref()
                .map(|path| catalog::credentials(path, &source, allow_http_auth))
                .transpose()?;
            let origin = url::Url::parse(&source)?.origin();
            let mut transport = catalog::HostTransport {
                endpoint: endpoint.map(|address| (origin, address)),
            };
            let mut interrupts = catalog::Interrupts::new()?;
            let operation = interrupts.begin();
            let result = femto_catalog::http::fetch_authenticated(
                &mut transport,
                &source,
                std::time::Instant::now() + std::time::Duration::from_secs(timeout),
                operation.flag(),
                auth.as_ref(),
            )?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Command::OpdsInspect { feed, source } => {
            use std::io::Read;
            let mut bytes = Vec::new();
            File::open(feed)?
                .take((femto_catalog::MAX_FEED_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            let catalog = femto_catalog::Catalog::parse(&bytes, &source)?;
            println!("{}", serde_json::to_string_pretty(&catalog)?);
        }
        Command::Inspect { book } => {
            let doc = Document::open(book)?;
            println!("{}", serde_json::to_string_pretty(&doc.metadata)?);
        }
        Command::Library { root, search } => {
            let library = Library::scan(root)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&library.search(&search))?
            );
            for error in &library.errors {
                eprintln!("{error}");
            }
        }
        Command::Render {
            raster,
            book,
            font,
            bold_font,
            italic_font,
            bold_italic_font,
            fallback_font,
            chapter,
            font_size,
            width,
            height,
            margin,
            output,
            levels,
        } => {
            let mut doc = Document::open(book)?;
            let fonts = Fonts {
                regular: Font::load(font)?,
                bold: bold_font.map(Font::load).transpose()?,
                italic: italic_font.map(Font::load).transpose()?,
                bold_italic: bold_italic_font.map(Font::load).transpose()?,
                fallback: fallback_font
                    .into_iter()
                    .map(Font::load)
                    .collect::<femto_reader::Result<Vec<_>>>()?,
            };
            let config = LayoutConfig {
                font_size,
                width,
                height,
                margin,
                ..Default::default()
            };
            let pages = paginate(&doc.chapter(chapter)?, &fonts, config)?;
            let text = raster.apply(TextRaster::default())?;
            fs::create_dir_all(&output)?;
            for (i, page) in pages.pages.iter().enumerate() {
                let rendered =
                    rasterize_display_report(page, &fonts, config, text, levels, |source| {
                        doc.resource(source)
                    })?;
                write_png(
                    File::create(output.join(format!("page-{i:04}.png")))?,
                    &rendered.pixels,
                    config,
                )?;
                for warning in rendered.warnings {
                    eprintln!("{warning}");
                }
            }
            println!("{}: {} pages", doc.metadata.title, pages.pages.len());
            for warning in pages.warnings {
                eprintln!("{warning}");
            }
        }
        Command::Read {
            raster,
            book,
            font,
            fallback_font,
            state,
            display,
            levels,
        } => {
            #[cfg(all(unix, not(target_os = "espidf")))]
            let state_file = state_file::StateFile::acquire(&state)?;
            #[cfg(all(unix, not(target_os = "espidf")))]
            let state = state_file.path().to_owned();
            let doc = Document::open(book)?;
            let mut fonts = Fonts::new(Font::load(font)?);
            fonts.fallback = fallback_font
                .into_iter()
                .map(Font::load)
                .collect::<femto_reader::Result<Vec<_>>>()?;
            let mut reader = Reader::new(doc, fonts, ReaderState::load(&state)?)?;
            reader.state.text_raster = raster.apply(reader.state.text_raster)?;
            if let Some(parent) = display.parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(parent)?;
            }
            loop {
                let pixels = reader.render(levels)?;
                write_png(File::create(&display)?, &pixels, reader.state.layout)?;
                reader.record_position(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
                reader.state.save(&state)?;
                println!(
                    "{} | {} | page {}/{}{} | {}",
                    reader.document.metadata.title,
                    reader.section_label(),
                    reader.page_index() + 1,
                    reader.page_count(),
                    reader
                        .book_progress_percent()
                        .map_or_else(String::new, |percent| format!(" | book ~{percent}%")),
                    display.display()
                );
                for warning in reader.warnings() {
                    eprintln!("{warning}");
                }
                println!(
                    "n next; p previous; b bookmark; + larger; - smaller; c chapters; t contents; l page links; m bookmarks; c/t/l/m N jump; back; q quit"
                );
                let mut command = String::new();
                if std::io::stdin().read_line(&mut command)? == 0 {
                    break;
                }
                let action = match command.trim() {
                    "q" => break,
                    "b" => Some(Action::ToggleBookmark),
                    "n" => Some(Action::Next),
                    "p" => Some(Action::Previous),
                    "back" => Some(Action::Back),
                    "+" | "-" => {
                        let mut config = reader.state.layout;
                        config.font_size = (config.font_size
                            + if command.trim() == "+" { 2.0 } else { -2.0 })
                        .clamp(8.0, 64.0);
                        Some(Action::Reflow(config))
                    }
                    "t" => {
                        for (i, entry) in reader.document.metadata.contents.iter().enumerate() {
                            println!("{}: {}", i + 1, entry.title);
                        }
                        None
                    }
                    "l" => {
                        for (i, entry) in reader.page_links().iter().enumerate() {
                            println!("{}: {} -> {}", i + 1, entry.label, entry.href);
                        }
                        None
                    }
                    "c" => {
                        for (i, href) in reader.document.metadata.chapters.iter().enumerate() {
                            println!("{}: {}", i + 1, href);
                        }
                        None
                    }
                    "m" => {
                        if let Some(book) = reader.state.books.get(&reader.document.metadata.id) {
                            for (i, mark) in book.bookmarks.iter().enumerate() {
                                println!("{}: {}", i + 1, mark.label);
                            }
                        }
                        None
                    }
                    command => {
                        if let Some((kind, number)) = command.split_once(' ')
                            && let Ok(number) = number.trim().parse::<usize>()
                            && let Some(index) = number.checked_sub(1)
                        {
                            match kind {
                                "c" => Some(Action::Chapter(index)),
                                "m" => Some(Action::Bookmark(index)),
                                "t" => Some(Action::Contents(index)),
                                "l" => Some(Action::Link(index)),
                                _ => {
                                    eprintln!("unknown command");
                                    None
                                }
                            }
                        } else {
                            eprintln!("unknown command");
                            None
                        }
                    }
                };
                if let Some(action) = action
                    && let Err(error) = reader.dispatch(action)
                {
                    eprintln!("{error}");
                }
            }
        }
    }
    Ok(())
}

fn parse_levels(value: &str) -> std::result::Result<u16, String> {
    match value.parse::<u16>() {
        Ok(levels @ (2 | 4 | 256)) => Ok(levels),
        _ => Err("display levels must be 2, 4, or 256".into()),
    }
}
