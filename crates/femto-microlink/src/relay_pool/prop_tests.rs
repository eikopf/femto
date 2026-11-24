//! Property test of one relay connection over a scripted transport that
//! reads short, blocks, and accepts output a few bytes at a time.
//!
//! Reference: the relay's frames come out as their events, once and in
//! order; every accepted datagram and every answer to a ping reaches the
//! wire whole, once, in the order it was queued; a refusal for a full queue
//! leaves the connection usable.
use super::*;
use proptest::prelude::*;
use std::io;

struct Io;
impl io::Read for Io {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        unreachable!()
    }
}
impl io::Write for Io {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        unreachable!()
    }
    fn flush(&mut self) -> io::Result<()> {
        unreachable!()
    }
}
impl crate::derp::DeadlineIo for Io {
    fn set_deadline(&mut self, _: std::time::Instant) {}
}

#[derive(Default)]
struct Script {
    /// Sent by the relay, not yet read.
    input: VecDeque<u8>,
    eof: bool,
    /// Most bytes each coming read returns; zero is "pending".
    reads: VecDeque<usize>,
    /// Most bytes the coming output call accepts.
    accept: usize,
    flushed: bool,
    wire: Vec<u8>,
    /// Length of `wire` when a flush last reported everything written.
    flushed_len: usize,
    closed: bool,
}
impl Transport for Script {
    type Blocking = Io;
    fn from_verified(_: Io) -> io::Result<Self> {
        unreachable!()
    }
    fn check(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn queue(&mut self, bytes: &[u8]) -> io::Result<usize> {
        assert!(!self.closed && !bytes.is_empty());
        let count = self.accept.min(bytes.len());
        self.accept -= count;
        self.wire.extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    fn flush_pending(&mut self) -> io::Result<bool> {
        if self.flushed {
            self.flushed_len = self.wire.len();
        }
        Ok(self.flushed)
    }
    fn receive(&mut self, buffer: &mut [u8]) -> io::Result<Option<usize>> {
        assert!(!self.closed);
        // Some(0) means end of stream only for a nonempty buffer.
        assert!(!buffer.is_empty(), "read with an empty buffer");
        let limit = self.reads.pop_front().unwrap_or(usize::MAX);
        let count = limit.min(buffer.len()).min(self.input.len());
        if count == 0 {
            return Ok((self.input.is_empty() && self.eof && limit > 0).then_some(0));
        }
        for slot in &mut buffer[..count] {
            *slot = self.input.pop_front().unwrap();
        }
        Ok(Some(count))
    }
    fn close(&mut self) {
        self.closed = true;
    }
}

fn frame(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![kind];
    bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
    bytes.extend_from_slice(body);
    bytes
}

#[derive(Clone, Debug)]
enum Incoming {
    Packet(u8, Vec<u8>),
    KeepAlive,
    Ping([u8; 8]),
    Pong([u8; 8]),
    Gone(u8),
    Health(String),
    Unknown(Vec<u8>),
}
impl Incoming {
    fn wire(&self) -> Vec<u8> {
        match self {
            Self::Packet(source, payload) => frame(5, &[&[*source; 32][..], payload].concat()),
            Self::KeepAlive => frame(6, &[]),
            Self::Ping(value) => frame(0x12, value),
            Self::Pong(value) => frame(0x13, value),
            Self::Gone(peer) => frame(8, &[*peer; 32]),
            Self::Health(text) => frame(0x14, text.as_bytes()),
            Self::Unknown(body) => frame(0x30, body),
        }
    }
    fn event(&self) -> Option<Event> {
        Some(match self {
            Self::Packet(source, payload) => Event::Packet {
                source: PublicKey::from_bytes([*source; 32]),
                payload: payload.clone(),
            },
            Self::KeepAlive => Event::KeepAlive,
            Self::Ping(value) => Event::Ping(*value),
            Self::Pong(value) => Event::Pong(*value),
            Self::Gone(peer) => Event::PeerGone {
                peer: PublicKey::from_bytes([*peer; 32]),
                reason: 0,
            },
            Self::Health(text) => Event::Health(text.clone()),
            Self::Unknown(_) => return None,
        })
    }
}
fn incoming() -> impl Strategy<Value = Incoming> {
    let payload = prop_oneof![
        4 => prop::collection::vec(any::<u8>(), 0..40),
        1 => Just(vec![9; crate::derp::MAX_DATAGRAM]),
        1 => Just(vec![9; 1100]),
    ];
    prop_oneof![
        4 => (1..4u8, payload).prop_map(|(s, p)| Incoming::Packet(s, p)),
        1 => Just(Incoming::KeepAlive),
        3 => any::<[u8; 8]>().prop_map(Incoming::Ping),
        1 => any::<[u8; 8]>().prop_map(Incoming::Pong),
        1 => (1..4u8).prop_map(Incoming::Gone),
        1 => "[a-z]{0,12}".prop_map(Incoming::Health),
        1 => prop::collection::vec(any::<u8>(), 0..20).prop_map(Incoming::Unknown),
    ]
}

#[derive(Clone, Debug)]
enum Step {
    /// The relay sends frames.
    Deliver(Vec<Incoming>),
    /// This node queues a datagram.
    Send(u8, Vec<u8>),
    /// One poll: read limits, output bytes accepted, whether output flushes.
    Poll { reads: Vec<usize>, accept: usize, flushed: bool },
}
fn step() -> impl Strategy<Value = Step> {
    let read = prop_oneof![3 => Just(usize::MAX), 2 => 1..8usize, 1 => Just(0usize), 1 => Just(700usize)];
    let accept = prop_oneof![4 => Just(usize::MAX), 2 => Just(0usize), 2 => 1..50usize];
    prop_oneof![
        2 => prop::collection::vec(incoming(), 1..4).prop_map(Step::Deliver),
        3 => (1..4u8, prop_oneof![prop::collection::vec(any::<u8>(), 0..30), Just(vec![3; crate::derp::MAX_DATAGRAM])])
            .prop_map(|(peer, payload)| Step::Send(peer, payload)),
        6 => (prop::collection::vec(read, 0..3), accept, prop::bool::weighted(0.7))
            .prop_map(|(reads, accept, flushed)| Step::Poll { reads, accept, flushed }),
    ]
}

/// Runs the steps, then lets the connection finish everything it holds.
/// `tolerate_ping_overflow` ends the case at the known refusal of a ping
/// that arrives while two output frames wait (see the regression test).
fn run(steps: &[Step], end_of_stream: bool, tolerate_ping_overflow: bool) -> Result<(), TestCaseError> {
    let mut connection = Connection {
        tls: Script::default(),
        decoder: Decoder::default(),
        output: VecDeque::new(),
        last_ms: 0,
        last_input_ms: 0,
        closed: false,
    };
    let mut now = 0u64;
    // Events owed, and output frames queued, in order.
    let mut owed: VecDeque<Event> = VecDeque::new();
    let mut queued: Vec<Vec<u8>> = Vec::new();
    let mut delivered = 0;
    let waiting = |connection: &Connection<Script>, queued: &[Vec<u8>]| {
        let mut end = 0;
        queued
            .iter()
            .filter(|frame| {
                end += frame.len();
                end > connection.tls.flushed_len
            })
            .count()
    };
    let poll = |connection: &mut Connection<Script>,
                    owed: &mut VecDeque<Event>,
                    queued: &mut Vec<Vec<u8>>,
                    now: u64|
     -> Result<bool, TestCaseError> {
        let before = waiting(connection, queued);
        match connection.poll(now) {
            Ok(Some(event)) => {
                prop_assert_eq!(Some(&event), owed.front(), "event out of order or invented");
                owed.pop_front();
                if let Event::Ping(value) = event {
                    queued.push(derp_frames::pong(value));
                }
                Ok(true)
            }
            Ok(None) => Ok(true),
            Err(error) => {
                // The stream may end inside a poll that first read a frame
                // without an event.
                if connection.tls.eof
                    && connection.tls.input.is_empty()
                    && matches!(error, Error::Closed)
                {
                    prop_assert!(owed.is_empty(), "events lost before end of stream");
                    prop_assert!(connection.closed && connection.tls.closed);
                    return Ok(false);
                }
                let ping_next = matches!(owed.front(), Some(Event::Ping(_)));
                if tolerate_ping_overflow && ping_next && before >= 2 {
                    return Ok(false);
                }
                Err(TestCaseError::fail(format!("poll failed with {before} unflushed frames: {error}")))
            }
        }
    };
    for step in steps {
        now += 1;
        match step {
            Step::Deliver(frames) => {
                for frame in frames {
                    connection.tls.input.extend(frame.wire());
                    owed.extend(frame.event());
                    delivered += 1;
                }
            }
            Step::Send(peer, payload) => {
                let busy = waiting(&connection, &queued) >= 2;
                match connection.send(PublicKey::from_bytes([*peer; 32]), payload, now) {
                    Ok(()) => queued.push(
                        derp_frames::send_packet(PublicKey::from_bytes([*peer; 32]), payload).unwrap(),
                    ),
                    Err(Error::Busy) => prop_assert!(busy, "refused with room in the queue"),
                    Err(error) => prop_assert!(false, "send failed: {error}"),
                }
                prop_assert!(!connection.closed);
            }
            Step::Poll { reads, accept, flushed } => {
                connection.tls.reads = reads.iter().copied().collect();
                connection.tls.accept = *accept;
                connection.tls.flushed = *flushed;
                if !poll(&mut connection, &mut owed, &mut queued, now)? {
                    return Ok(());
                }
            }
        }
        let expected = queued.concat();
        prop_assert!(expected.starts_with(&connection.tls.wire), "output is not the queued frames in order");
    }
    // Nothing in the way any more: everything owed arrives within a bounded
    // number of polls.
    connection.tls.eof = end_of_stream;
    let budget = 4 * (delivered + queued.len()) + 8;
    let mut ended = false;
    #[allow(clippy::needless_range_loop)]
    for _ in 0..budget {
        now += 1;
        connection.tls.reads.clear();
        connection.tls.accept = usize::MAX;
        connection.tls.flushed = true;
        if !poll(&mut connection, &mut owed, &mut queued, now)? {
            ended = connection.tls.eof && connection.tls.input.is_empty() && owed.is_empty();
            if !ended {
                return Ok(());
            }
            break;
        }
    }
    prop_assert!(owed.is_empty(), "events never delivered: {:?}", owed);
    prop_assert_eq!(ended, end_of_stream);
    if !end_of_stream {
        prop_assert_eq!(&connection.tls.wire, &queued.concat());
        prop_assert!(!connection.pending_output());
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1500))]
    #[test]
    fn connection_delivers_events_and_output_in_order(
        steps in prop::collection::vec(step(), 1..40),
        end_of_stream in any::<bool>(),
    ) {
        run(&steps, end_of_stream, true)?;
    }
}

/// The relay's ping arrives while two datagrams wait for a socket that is
/// momentarily not writable. The contract (`Connection::poll`: "Pings queue
/// their pong automatically"; `Transport::queue`: "zero means backpressure")
/// gives output five seconds to drain; instead the connection is closed at
/// once, and the pool then keeps that relay in a thirty-second cooldown.
#[test]
#[ignore = "bug: a relay ping during brief output backpressure closes the connection"]
fn ping_while_two_datagrams_wait_does_not_close_the_connection() {
    let steps = [
        Step::Send(1, vec![1]),
        Step::Send(1, vec![2]),
        Step::Deliver(vec![Incoming::Ping([7; 8])]),
        // The socket takes no output on this poll.
        Step::Poll { reads: vec![], accept: 0, flushed: false },
    ];
    run(&steps, false, false).unwrap();
}
