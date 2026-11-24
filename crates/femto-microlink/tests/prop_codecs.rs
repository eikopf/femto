//! Property tests for the relay frame decoder and the coordinator map stream.
//!
//! Both decoders are incremental. Their reference models here parse the whole
//! byte string at once, so any dependence on how the bytes were cut into
//! reads shows up as a difference.
use femto_microlink::{
    control_stream::{MAX_MAP_BYTES, MapStream, StreamError},
    derp::MAX_DATAGRAM,
    derp_frames::{self, Decoder},
    protocol::{
        derp::Event,
        key::{Node, PublicKey},
    },
};
use proptest::prelude::*;

// ---------------------------------------------------------------- relay frames

/// What a whole-buffer parse of `wire` yields: the events of every complete
/// valid frame, then how the stream ends.
#[derive(Debug, PartialEq)]
enum End {
    /// Ends on a frame boundary.
    Clean,
    /// Ends inside a frame whose header (if complete) is valid.
    Partial,
    /// A frame is invalid; decoding stops no later than its last byte.
    Invalid,
}

fn header_ok(kind: u8, length: usize) -> bool {
    match kind {
        1..=4 => false,
        5 => (32..=32 + MAX_DATAGRAM).contains(&length),
        6 => length == 0,
        8 => length == 32 || length == 33,
        0x12 | 0x13 | 0x15 => length == 8,
        0x14 => length <= 1024,
        _ => length <= 256,
    }
}

fn key(bytes: &[u8]) -> PublicKey<Node> {
    PublicKey::from_bytes(bytes.try_into().unwrap())
}

fn model(wire: &[u8]) -> (Vec<Option<Event>>, End) {
    let mut events = Vec::new();
    let mut at = 0;
    loop {
        let rest = &wire[at..];
        if rest.is_empty() {
            return (events, End::Clean);
        }
        if rest.len() < 5 {
            return (events, End::Partial);
        }
        let kind = rest[0];
        let length = u32::from_be_bytes([rest[1], rest[2], rest[3], rest[4]]) as usize;
        if !header_ok(kind, length) {
            return (events, End::Invalid);
        }
        if rest.len() < 5 + length {
            return (events, End::Partial);
        }
        let body = &rest[5..5 + length];
        at += 5 + length;
        events.push(match kind {
            5 => Some(Event::Packet {
                source: key(&body[..32]),
                payload: body[32..].to_vec(),
            }),
            6 => Some(Event::KeepAlive),
            8 => Some(Event::PeerGone {
                peer: key(&body[..32]),
                reason: if length == 33 { body[32] } else { 0 },
            }),
            0x12 => Some(Event::Ping(body.try_into().unwrap())),
            0x13 => Some(Event::Pong(body.try_into().unwrap())),
            0x14 => match std::str::from_utf8(body) {
                Ok(text) => Some(Event::Health(text.to_owned())),
                Err(_) => return (events, End::Invalid),
            },
            0x15 => Some(Event::Restarting {
                reconnect_in_ms: u32::from_be_bytes(body[..4].try_into().unwrap()),
                try_for_ms: u32::from_be_bytes(body[4..].try_into().unwrap()),
            }),
            _ => None,
        });
    }
}

fn frame(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![kind];
    bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
    bytes.extend_from_slice(body);
    bytes
}

fn body(range: impl Into<prop::collection::SizeRange>) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), range)
}

/// Valid frames of every kind, with lengths at each kind's edges.
fn valid_frame() -> BoxedStrategy<Vec<u8>> {
    prop_oneof![
        3 => prop_oneof![Just(32usize), Just(33), Just(32 + MAX_DATAGRAM), 32..200usize, 32..=32 + MAX_DATAGRAM]
            .prop_flat_map(body).prop_map(|b| frame(5, &b)),
        2 => Just(frame(6, &[])),
        2 => body(32..=33).prop_map(|b| frame(8, &b)),
        2 => body(8).prop_map(|b| frame(0x12, &b)),
        1 => body(8).prop_map(|b| frame(0x13, &b)),
        1 => "[a-z é]{0,40}".prop_map(|s| frame(0x14, s.as_bytes())),
        1 => Just(frame(0x14, &[b'h'; 1024])),
        1 => body(8).prop_map(|b| frame(0x15, &b)),
        2 => (prop_oneof![Just(0u8), Just(7), Just(9), 0x16..=0xffu8], prop_oneof![Just(0usize), Just(256), 0..40usize])
            .prop_flat_map(|(k, n)| body(n).prop_map(move |b| frame(k, &b))),
    ].boxed()
}

/// Frames broken in one respect: a length just outside the kind's range, a
/// handshake-only kind, or a health text that is not UTF-8.
fn invalid_frame() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        (1..=4u8, 0..300usize).prop_map(|(k, n)| frame(k, &vec![0; n])),
        prop_oneof![Just(31usize), Just(33 + MAX_DATAGRAM), Just(0)]
            .prop_map(|n| frame(5, &vec![1; n])),
        Just(frame(6, &[0])),
        prop_oneof![Just(31usize), Just(34)].prop_map(|n| frame(8, &vec![1; n])),
        (
            prop_oneof![Just(0x12u8), Just(0x13), Just(0x15)],
            prop_oneof![Just(7usize), Just(9)]
        )
            .prop_map(|(k, n)| frame(k, &vec![1; n])),
        Just(frame(0x14, &[b'h'; 1025])),
        Just(frame(0x14, &[0xff, 0xfe])),
        Just(frame(0x20, &[0; 257])),
        any::<u32>().prop_map(|n| {
            let mut bytes = vec![5];
            bytes.extend_from_slice(&n.max(40_000).to_be_bytes());
            bytes
        }),
    ]
}

fn wire() -> impl Strategy<Value = Vec<u8>> {
    let frames = prop::collection::vec(valid_frame(), 0..8)
        .prop_map(|f| f.concat())
        .boxed();
    prop_oneof![
        4 => frames.clone(),
        // valid frames, one invalid frame, more bytes
        3 => (frames.clone(), invalid_frame(), body(0..20))
            .prop_map(|(a, b, c)| [a, b, c].concat()),
        // a truncated tail
        2 => (frames.clone(), valid_frame(), any::<prop::sample::Index>())
            .prop_map(|(a, b, cut)| { let n = cut.index(b.len()); [a, b[..n].to_vec()].concat() }),
        1 => body(0..64),
    ]
}

/// Cut points for delivering `wire` in pieces, from bytewise to whole.
fn cuts(len: usize) -> impl Strategy<Value = Vec<usize>> {
    prop_oneof![
        Just(vec![]),
        Just((1..len.max(1)).collect()),
        prop::collection::vec(0..=len, 0..12).prop_map(|mut c| {
            c.sort_unstable();
            c
        }),
    ]
}

fn chunks<'a>(wire: &'a [u8], cuts: &[usize]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    let mut last = 0;
    for &cut in cuts.iter().chain([&wire.len()]) {
        if cut > last {
            out.push(&wire[last..cut]);
            last = cut;
        }
    }
    out
}

/// Runs the decoder as its caller does: one frame per call, unconsumed bytes
/// offered again. Returns the events, the bytes consumed and the ending.
fn decode(pieces: &[&[u8]]) -> (Vec<Option<Event>>, usize, End) {
    let mut decoder = Decoder::default();
    let mut events = Vec::new();
    let mut consumed = 0;
    for piece in pieces {
        let mut rest: &[u8] = piece;
        while !rest.is_empty() {
            // `wanted` is how much the decoder asks its reader for; it is
            // never zero while the decoder is open.
            assert!(decoder.wanted() > 0);
            match decoder.feed(rest, 0) {
                Ok((used, event)) => {
                    assert!(used > 0 && used <= rest.len(), "used {used}");
                    consumed += used;
                    events.push((used, event));
                    rest = &rest[used..];
                }
                Err(_) => {
                    // Errors are terminal.
                    assert!(decoder.feed(&[6, 0, 0, 0, 0], 0).is_err());
                    assert!(decoder.maintain(0).is_err());
                    let events = events
                        .into_iter()
                        .filter_map(|(_, e)| e)
                        .map(Some)
                        .collect();
                    return (events, consumed, End::Invalid);
                }
            }
        }
    }
    let end = if decoder.eof().is_ok() {
        End::Clean
    } else {
        End::Partial
    };
    let events = events
        .into_iter()
        .filter_map(|(_, e)| e)
        .map(Some)
        .collect();
    (events, consumed, end)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// However the bytes are cut, the decoder yields the events of a
    /// whole-buffer parse, stops at the first invalid frame, and reports a
    /// truncated tail at end of input.
    #[test]
    fn relay_decoder_matches_whole_buffer_parse(
        (wire, cuts) in wire().prop_flat_map(|w| { let n = w.len(); (Just(w), cuts(n)) })
    ) {
        let (expected, end) = model(&wire);
        let expected: Vec<_> = expected.into_iter().flatten().map(Some).collect();
        let (events, consumed, got_end) = decode(&chunks(&wire, &cuts));
        prop_assert_eq!(&events, &expected);
        prop_assert_eq!(&got_end, &end);
        if end != End::Invalid {
            prop_assert_eq!(consumed, wire.len());
        }
    }

    /// A frame that is still incomplete five seconds after its first byte is
    /// refused; frames that complete sooner are not, however long the idle
    /// gaps between frames.
    #[test]
    fn relay_decoder_times_out_only_stalled_frames(
        frames in prop::collection::vec((valid_frame(), any::<prop::sample::Index>(), 0..7000u64, 0..20_000u64), 1..6)
    ) {
        let mut decoder = Decoder::default();
        let mut now = 0u64;
        for (frame, cut, stall, idle) in frames {
            // Idle time between frames never times out.
            now += idle;
            prop_assert!(decoder.maintain(now).is_ok());
            let cut = 1 + cut.index(frame.len() - 1);
            let (used, event) = decoder.feed(&frame[..cut], now).unwrap();
            prop_assert_eq!((used, event), (cut, None));
            now += stall;
            let result = decoder.feed(&frame[cut..], now);
            if stall >= 5000 {
                prop_assert!(result.is_err());
                return Ok(());
            }
            let (used, _) = result.unwrap();
            prop_assert_eq!(used, frame.len() - cut);
            prop_assert_eq!(decoder.wanted(), 5);
        }
    }

    /// What this node sends parses as the frame it meant.
    #[test]
    fn relay_send_frame_layout(peer in any::<[u8; 32]>(), packet in body(0..=MAX_DATAGRAM + 2)) {
        let result = derp_frames::send_packet(PublicKey::from_bytes(peer), &packet);
        if packet.len() > MAX_DATAGRAM {
            prop_assert!(result.is_err());
        } else {
            let bytes = result.unwrap();
            prop_assert_eq!(bytes[0], 4);
            prop_assert_eq!(u32::from_be_bytes(bytes[1..5].try_into().unwrap()) as usize, bytes.len() - 5);
            prop_assert_eq!(&bytes[5..37], &peer[..]);
            prop_assert_eq!(&bytes[37..], &packet[..]);
        }
    }
}

// ------------------------------------------------------------------ map stream

#[derive(Debug, PartialEq)]
enum MapEnd {
    Clean,
    Truncated,
    Length,
    Handler,
}

fn map_model(wire: &[u8], fail_on: Option<usize>) -> (Vec<Vec<u8>>, MapEnd) {
    let mut maps = Vec::new();
    let mut rest = wire;
    loop {
        if rest.is_empty() {
            return (maps, MapEnd::Clean);
        }
        if rest.len() < 4 {
            return (maps, MapEnd::Truncated);
        }
        let length = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        if length == 0 || length > MAX_MAP_BYTES {
            return (maps, MapEnd::Length);
        }
        if rest.len() < 4 + length {
            return (maps, MapEnd::Truncated);
        }
        maps.push(rest[4..4 + length].to_vec());
        if fail_on == Some(maps.len() - 1) {
            return (maps, MapEnd::Handler);
        }
        rest = &rest[4 + length..];
    }
}

fn map_frame(body: &[u8]) -> Vec<u8> {
    let mut bytes = (body.len() as u32).to_le_bytes().to_vec();
    bytes.extend_from_slice(body);
    bytes
}

fn map_wire() -> impl Strategy<Value = Vec<u8>> {
    let one = prop_oneof![
        6 => body(1..40).prop_map(|b| map_frame(&b)),
        1 => Just(map_frame(&vec![7; MAX_MAP_BYTES])),
        1 => Just(map_frame(&[1])),
    ]
    .boxed();
    let frames = prop::collection::vec(one.clone(), 0..6)
        .prop_map(|f| f.concat())
        .boxed();
    let bad = prop_oneof![
        Just(0u32.to_le_bytes().to_vec()),
        Just((MAX_MAP_BYTES as u32 + 1).to_le_bytes().to_vec()),
        any::<u32>().prop_map(|n| n.max(MAX_MAP_BYTES as u32 + 1).to_le_bytes().to_vec()),
    ];
    prop_oneof![
        4 => frames.clone(),
        2 => (frames.clone(), bad, body(0..10)).prop_map(|(a, b, c)| [a, b, c].concat()),
        2 => (frames, one, any::<prop::sample::Index>())
            .prop_map(|(a, b, cut)| { let n = cut.index(b.len()); [a, b[..n].to_vec()].concat() }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(600))]

    #[test]
    fn map_stream_matches_whole_buffer_parse(
        (wire, cuts) in map_wire().prop_flat_map(|w| { let n = w.len(); (Just(w), cuts(n)) }),
        fail_on in prop::option::weighted(0.3, 0..6usize),
    ) {
        let (expected, end) = map_model(&wire, fail_on);
        let mut stream = MapStream::default();
        let mut seen: Vec<Vec<u8>> = Vec::new();
        let mut error = None;
        for piece in chunks(&wire, &cuts) {
            let result = stream.push(piece, |map| {
                seen.push(map.to_vec());
                if fail_on == Some(seen.len() - 1) { Err("refused") } else { Ok(()) }
            });
            if let Err(e) = result {
                error = Some(e);
                break;
            }
        }
        prop_assert_eq!(&seen, &expected);
        match end {
            MapEnd::Clean => {
                prop_assert_eq!(error, None);
                prop_assert_eq!(stream.finish::<&str>(), Ok(()));
            }
            MapEnd::Truncated => {
                prop_assert_eq!(error, None);
                prop_assert_eq!(stream.finish::<&str>(), Err(StreamError::Truncated));
            }
            MapEnd::Length => prop_assert_eq!(error, Some(StreamError::Length)),
            MapEnd::Handler => prop_assert_eq!(error, Some(StreamError::Handler("refused"))),
        }
        // Every ending closes the stream for good.
        let mut called = false;
        prop_assert_eq!(
            stream.push(&map_frame(b"{}"), |_| { called = true; Ok::<_, &str>(()) }),
            Err(StreamError::Closed)
        );
        prop_assert!(!called);
        prop_assert_eq!(stream.finish::<&str>(), Err(StreamError::Closed));
    }
}
