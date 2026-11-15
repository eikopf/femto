//! Bounded commands for a physically connected USB console.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Status,
    Next,
    Previous,
    Select,
    Back,
}

#[derive(Default)]
pub struct Parser {
    bytes: [u8; 24],
    len: usize,
    overflow: bool,
}

impl Parser {
    pub fn push(&mut self, byte: u8) -> Option<Command> {
        if matches!(byte, b'\r' | b'\n') {
            let command = if self.overflow {
                None
            } else {
                match &self.bytes[..self.len] {
                    b"femto status" => Some(Command::Status),
                    b"femto next" => Some(Command::Next),
                    b"femto prev" => Some(Command::Previous),
                    b"femto select" => Some(Command::Select),
                    b"femto back" => Some(Command::Back),
                    _ => None,
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

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
        let mut parser = Parser::default();
        for byte in [b'x'; 1000]
            .iter()
            .chain(b"femto next\nnext\nfemto back\0\n")
        {
            assert_eq!(parser.push(*byte), None);
        }
        let commands: Vec<_> = b"femto select\nfemto back\n"
            .iter()
            .filter_map(|b| parser.push(*b))
            .collect();
        assert_eq!(commands, [Command::Select, Command::Back]);
    }
}
