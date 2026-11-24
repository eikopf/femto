//! Property tests for the USB console parser.
use femto_picco_board::{
    console::{Command, MAX_FILE_BYTES, Parser, crc32},
    input::Turn,
};
use proptest::prelude::*;

fn commands(input: &[u8]) -> Vec<Command> {
    let mut parser = Parser::default();
    input.iter().filter_map(|b| parser.push(*b)).collect()
}

/// The line a host would type for a command.
fn print(command: &Command) -> String {
    let text = match command {
        Command::Status => "status".into(),
        Command::Next => "next".into(),
        Command::Previous => "prev".into(),
        Command::Select => "select".into(),
        Command::Back => "back".into(),
        Command::Power => "power".into(),
        Command::Awake => "awake".into(),
        Command::Relay => "relay".into(),
        Command::Tasks => "tasks".into(),
        Command::Fault => "fault".into(),
        Command::Sleep(seconds) => format!("sleep {seconds}"),
        Command::Swipe(Turn::Next) => "swipe next".into(),
        Command::Swipe(Turn::Previous) => "swipe prev".into(),
        Command::Slide { up: true } => "swipe up".into(),
        Command::Slide { up: false } => "swipe down".into(),
        Command::Hold => "hold".into(),
        Command::Tap { x, y } => format!("tap {x} {y}"),
        Command::Type(text) => format!("type {text}"),
        Command::Choose(text) => format!("choose {text}"),
        Command::Frame => "frame".into(),
        Command::Put { len, crc, path } => format!("put {len} {crc:08X} {path}"),
        Command::List(path) if path.is_empty() => "ls".into(),
        Command::List(path) => format!("ls {path}"),
        Command::Show(path) => format!("cat {path}"),
        Command::Remove(path) => format!("rm {path}"),
        Command::Reboot => "reboot".into(),
        Command::Update => "update".into(),
    };
    format!("femto {text}")
}

fn component() -> impl Strategy<Value = String> {
    prop_oneof![
        6 => "[a-zA-Z0-9_ .\u{e4}\u{20ac}-]{1,12}",
        1 => "\\PC{1,8}",
    ]
    .prop_filter("a path component", |part| {
        !matches!(part.as_str(), "." | "..") && !part.contains(['/', '\\'])
    })
}

fn safe_path() -> impl Strategy<Value = String> {
    prop::collection::vec(component(), 1..4).prop_map(|parts| parts.join("/"))
}

fn text() -> impl Strategy<Value = String> {
    prop_oneof!["[ -~]{1,40}", "\\PC{1,20}"]
}

fn command() -> impl Strategy<Value = Command> {
    prop_oneof![
        prop::sample::select(vec![
            Command::Status,
            Command::Next,
            Command::Previous,
            Command::Select,
            Command::Back,
            Command::Power,
            Command::Awake,
            Command::Relay,
            Command::Tasks,
            Command::Fault,
            Command::Swipe(Turn::Next),
            Command::Swipe(Turn::Previous),
            Command::Slide { up: true },
            Command::Slide { up: false },
            Command::Hold,
            Command::Frame,
            Command::Reboot,
            Command::Update,
            Command::List(String::new()),
        ]),
        (5u16..).prop_map(Command::Sleep),
        (any::<u16>(), any::<u16>()).prop_map(|(x, y)| Command::Tap { x, y }),
        text().prop_map(Command::Type),
        text().prop_map(Command::Choose),
        (0..=MAX_FILE_BYTES, any::<u32>(), safe_path())
            .prop_map(|(len, crc, path)| Command::Put { len, crc, path }),
        safe_path().prop_map(Command::List),
        safe_path().prop_map(Command::Show),
        safe_path().prop_map(Command::Remove),
    ]
}

/// Resolves a path below a base directory the way a filesystem would, with
/// either separator. Returns false if any prefix climbs above the base, if the
/// path is rooted, or if it carries characters a line protocol cannot hold.
fn stays_below(path: &str) -> bool {
    if path.starts_with(['/', '\\']) || path.chars().any(char::is_control) {
        return false;
    }
    let mut depth = 0i32;
    for part in path.split(['/', '\\']) {
        match part {
            ".." => depth -= 1,
            "." | "" => {}
            _ => depth += 1,
        }
        if depth < 0 {
            return false;
        }
    }
    depth > 0
}

fn path_of(command: &Command) -> Option<&str> {
    match command {
        Command::Put { path, .. }
        | Command::Show(path)
        | Command::Remove(path)
        | Command::List(path) => Some(path),
        _ => None,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    #[test]
    fn printed_commands_parse_back(command in command(), ending in prop::sample::select(vec!["\n", "\r", "\r\n"])) {
        let line = print(&command);
        prop_assume!(line.len() <= 192);
        prop_assert_eq!(commands(format!("{line}{ending}").as_bytes()), [command]);
    }

    /// Hostile path arguments: accepted ones stay below the data directory and
    /// arrive unchanged.
    #[test]
    fn accepted_paths_cannot_escape(
        verb in prop::sample::select(vec!["put 1 0 ", "ls ", "cat ", "rm "]),
        parts in prop::collection::vec(
            prop_oneof![
                3 => component(),
                2 => prop::sample::select(vec!["..", ".", "", " ", "...", ".. ", " ..", "\\", "..\\x", "a\\..", "C:", "\u{7f}", "\u{85}", "\u{0}", "\t", "%2e%2e"]).prop_map(String::from),
            ],
            1..5,
        ),
        rooted in prop::bool::weighted(0.1),
    ) {
        let path = format!("{}{}", if rooted { "/" } else { "" }, parts.join("/"));
        let line = format!("femto {verb}{path}\n");
        prop_assume!(line.len() <= 193);
        let parsed = commands(line.as_bytes());
        prop_assert!(parsed.len() <= 1);
        // A bare `ls` lists the data directory itself.
        prop_assume!(!(verb == "ls " && path.is_empty()));
        if let Some(command) = parsed.first() {
            let accepted = path_of(command).unwrap();
            prop_assert_eq!(accepted, path.as_str());
            prop_assert!(stays_below(accepted), "accepted {:?}", accepted);
            prop_assert!(accepted.split('/').all(|part| !matches!(part, "" | "." | "..")));
        } else if !path.contains('\n') && !path.contains('\r') {
            // Rejection needs a reason: an escaping, rooted, unnormalized or
            // unprintable path.
            let clean = stays_below(&path)
                && !path.contains('\\')
                && path.split('/').all(|part| !matches!(part, "" | "." | ".."));
            prop_assert!(!clean, "rejected {:?}", path);
        }
    }

    /// Arbitrary bytes never panic; whatever is accepted is a well-formed
    /// command; and the stream is cut into lines independently of each other.
    #[test]
    fn arbitrary_bytes_parse_line_by_line(
        chunks in prop::collection::vec(
            prop_oneof![
                3 => prop::collection::vec(any::<u8>(), 0..40),
                3 => command().prop_map(|c| print(&c).into_bytes()),
                2 => (command(), any::<prop::sample::Index>(), any::<u8>()).prop_map(|(c, at, byte)| {
                    let mut line = print(&c).into_bytes();
                    let at = at.index(line.len());
                    line[at] = byte;
                    line
                }),
                1 => (command(), "[ -~\u{0}]{0,3}").prop_map(|(c, suffix)| format!("{}{suffix}", print(&c)).into_bytes()),
                1 => prop::collection::vec(any::<u8>(), 180..260),
            ],
            0..8,
        ),
    ) {
        let mut stream = Vec::new();
        let mut want = Vec::new();
        for chunk in &chunks {
            stream.extend_from_slice(chunk);
            stream.push(b'\n');
        }
        for line in stream.split(|b| matches!(b, b'\r' | b'\n')) {
            let mut one = line.to_vec();
            one.push(b'\n');
            let parsed = commands(&one);
            prop_assert!(parsed.len() <= 1);
            if line.len() > 192 || core::str::from_utf8(line).is_err() {
                prop_assert!(parsed.is_empty());
            }
            for command in &parsed {
                // An accepted line reads the same after printing, up to number
                // formatting, so nothing was dropped or invented.
                let printed = print(command);
                prop_assert_eq!(commands(format!("{printed}\n").as_bytes()), [command.clone()]);
                match command {
                    Command::Sleep(seconds) => prop_assert!(*seconds >= 5),
                    Command::Put { len, .. } => prop_assert!(*len <= MAX_FILE_BYTES),
                    Command::Type(text) | Command::Choose(text) => {
                        prop_assert!(!text.is_empty() && !text.chars().any(char::is_control))
                    }
                    _ => {}
                }
                if let Some(path) = path_of(command) {
                    prop_assert!(path.is_empty() && matches!(command, Command::List(_)) || stays_below(path));
                }
                let text = core::str::from_utf8(line).unwrap();
                prop_assert!(text.starts_with("femto "));
                if !matches!(command, Command::Sleep(_) | Command::Tap { .. } | Command::Put { .. }) {
                    prop_assert_eq!(text.trim_end_matches(' '), printed.trim_end_matches(' '));
                }
            }
            want.extend(parsed);
        }
        prop_assert_eq!(commands(&stream), want);
    }

    #[test]
    fn crc_chains_and_matches_bitwise_reference(data in prop::collection::vec(any::<u8>(), 0..300), cut in any::<prop::sample::Index>()) {
        let mut reference = 0xffff_ffffu32;
        for byte in &data {
            reference ^= u32::from(*byte);
            for _ in 0..8 {
                reference = if reference & 1 == 1 { (reference >> 1) ^ 0xEDB8_8320 } else { reference >> 1 };
            }
        }
        prop_assert_eq!(crc32(0, &data), !reference);
        let cut = cut.index(data.len() + 1);
        prop_assert_eq!(crc32(crc32(0, &data[..cut]), &data[cut..]), !reference);
    }
}
