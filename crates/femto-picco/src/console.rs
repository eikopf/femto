//! Bounded commands for a physically connected USB console.

use alloc::string::String;

/// Largest accepted upload; the SD card is the only destination.
pub const MAX_FILE_BYTES: u32 = 256 * 1024 * 1024;
/// Upload payload bytes between acknowledgements.
pub const CHUNK_BYTES: usize = 2048;
const LINE_BYTES: usize = 192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Status,
    Next,
    Previous,
    Select,
    Back,
    Power,
    /// Suppress automatic sleep until restart, for unattended test sessions.
    Awake,
    Swipe(crate::input::Turn),
    /// A vertical swipe on the reading page.
    Slide {
        up: bool,
    },
    /// A touch held in place on the reading page.
    Hold,
    Tap {
        x: u16,
        y: u16,
    },
    /// Enter text into the open on-screen keyboard field.
    Type(String),
    /// Activate the entry of the open menu whose label starts with the text.
    Choose(String),
    /// Dump the last presented framebuffer.
    Frame,
    /// Receive a file below the device data directory.
    Put {
        len: u32,
        crc: u32,
        path: String,
    },
    List(String),
    Remove(String),
    Reboot,
    /// Install the firmware package waiting on the card.
    Update,
}

/// Accepts relative paths that cannot leave the directory they are joined to.
fn path(text: &str) -> Option<String> {
    let valid = !text.is_empty()
        && text.split('/').all(|part| {
            !matches!(part, "" | "." | "..") && !part.chars().any(|c| c.is_control() || c == '\\')
        });
    valid.then(|| text.into())
}

fn parse(line: &str) -> Option<Command> {
    let rest = line.strip_prefix("femto ")?;
    let (name, arguments) = rest.split_once(' ').unwrap_or((rest, ""));
    Some(match (name, arguments) {
        ("status", "") => Command::Status,
        ("next", "") => Command::Next,
        ("prev", "") => Command::Previous,
        ("select", "") => Command::Select,
        ("back", "") => Command::Back,
        ("power", "") => Command::Power,
        ("awake", "") => Command::Awake,
        ("swipe", "next") => Command::Swipe(crate::input::Turn::Next),
        ("swipe", "prev") => Command::Swipe(crate::input::Turn::Previous),
        ("swipe", "up") => Command::Slide { up: true },
        ("swipe", "down") => Command::Slide { up: false },
        ("hold", "") => Command::Hold,
        ("frame", "") => Command::Frame,
        ("reboot", "") => Command::Reboot,
        ("update", "") => Command::Update,
        ("tap", point) => {
            let (x, y) = point.split_once(' ')?;
            Command::Tap {
                x: x.parse().ok()?,
                y: y.parse().ok()?,
            }
        }
        ("type", text) if !text.is_empty() && !text.chars().any(char::is_control) => {
            Command::Type(text.into())
        }
        ("choose", text) if !text.is_empty() && !text.chars().any(char::is_control) => {
            Command::Choose(text.into())
        }
        ("put", arguments) => {
            let mut fields = arguments.splitn(3, ' ');
            let len = fields
                .next()?
                .parse()
                .ok()
                .filter(|&n| n <= MAX_FILE_BYTES)?;
            let crc = u32::from_str_radix(fields.next()?, 16).ok()?;
            Command::Put {
                len,
                crc,
                path: path(fields.next()?)?,
            }
        }
        ("ls", "") => Command::List(String::new()),
        ("ls", directory) => Command::List(path(directory)?),
        ("rm", file) => Command::Remove(path(file)?),
        _ => return None,
    })
}

pub struct Parser {
    bytes: [u8; LINE_BYTES],
    len: usize,
    overflow: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Self {
            bytes: [0; LINE_BYTES],
            len: 0,
            overflow: false,
        }
    }
}

impl Parser {
    pub fn push(&mut self, byte: u8) -> Option<Command> {
        if matches!(byte, b'\r' | b'\n') {
            let command = if self.overflow {
                None
            } else {
                core::str::from_utf8(&self.bytes[..self.len])
                    .ok()
                    .and_then(parse)
            };
            self.len = 0;
            self.overflow = false;
            return command;
        }
        if self.len == self.bytes.len() {
            self.overflow = true;
        } else {
            self.bytes[self.len] = byte;
            self.len += 1;
        }
        None
    }
}

/// IEEE CRC-32 over upload payloads; `state` starts at zero.
pub fn crc32(state: u32, data: &[u8]) -> u32 {
    let mut crc = !state;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn commands(input: &[u8]) -> Vec<Command> {
        let mut parser = Parser::default();
        input.iter().filter_map(|b| parser.push(*b)).collect()
    }

    #[test]
    fn fragmented_commands_and_crlf_execute_once() {
        let mut parser = Parser::default();
        for byte in b"femto ne" {
            assert_eq!(parser.push(*byte), None);
        }
        let commands: Vec<_> = b"xt\r\nfemto prev\nfemto status\n"
            .iter()
            .filter_map(|b| parser.push(*b))
            .collect();
        assert_eq!(
            commands,
            [Command::Next, Command::Previous, Command::Status]
        );
    }

    #[test]
    fn overflow_and_untrusted_suffixes_are_discarded_until_newline() {
        let mut input = [b'x'; 1000].to_vec();
        input.extend(b"femto next\nnext\nfemto back\0\nfemto next now\n");
        input.extend(b"femto select\nfemto back\n");
        assert_eq!(commands(&input), [Command::Select, Command::Back]);
    }

    #[test]
    fn arguments_are_bounded_and_paths_stay_relative() {
        assert_eq!(
            commands("femto power\nfemto update\nfemto swipe prev\n".as_bytes()),
            [
                Command::Power,
                Command::Update,
                Command::Swipe(crate::input::Turn::Previous)
            ]
        );
        assert_eq!(
            commands("femto tap 12 479\nfemto put 5 0000ABCD b\u{fc}cher/a b.epub\n".as_bytes()),
            [
                Command::Tap { x: 12, y: 479 },
                Command::Put {
                    len: 5,
                    crc: 0xabcd,
                    path: "b\u{fc}cher/a b.epub".into()
                }
            ]
        );
        assert_eq!(
            commands(b"femto type http://host:80/a b\n"),
            [Command::Type("http://host:80/a b".into())]
        );
        assert_eq!(
            commands(b"femto swipe up\nfemto swipe down\nfemto hold\n"),
            [
                Command::Slide { up: true },
                Command::Slide { up: false },
                Command::Hold
            ]
        );
        assert_eq!(
            commands(b"femto choose This book\n"),
            [Command::Choose("This book".into())]
        );
        assert_eq!(
            commands(b"femto ls\nfemto ls fonts\nfemto rm wifi.json\n"),
            [
                Command::List(String::new()),
                Command::List("fonts".into()),
                Command::Remove("wifi.json".into())
            ]
        );
        for rejected in [
            "femto put 5 0 /etc/x",
            "femto put 5 0 a/../x",
            "femto put 5 0 a//x",
            "femto put 5 0 a\\x",
            "femto put 268435457 0 x",
            "femto put 5 xyz x",
            "femto put 5 0",
            "femto rm",
            "femto rm ..",
            "femto ls ./x",
            "femto tap 70000 1",
            "femto tap 1",
            "femto type",
            "femto swipe",
            "femto swipe left",
        ] {
            let mut line = rejected.as_bytes().to_vec();
            line.push(b'\n');
            assert_eq!(commands(&line), [], "{rejected}");
        }
    }

    #[test]
    fn crc_matches_reference_and_chains() {
        assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(crc32(0, b"1234"), b"56789"), 0xCBF4_3926);
    }
}
