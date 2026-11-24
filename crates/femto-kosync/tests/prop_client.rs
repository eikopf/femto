//! Server replies, built field by field from valid and spoiled values, are
//! accepted exactly when an independent reading of the wire contract accepts
//! them; uploads are sent whole or not at all; document identities follow a
//! plain restatement of KOReader's sampling.
use femto_catalog::{
    deadline::Limit,
    http::{Stream, Transport, Url},
};
use femto_kosync::{Client, Credentials, DocumentId, Progress, config::Settings};
use md5::{Digest, Md5};
use proptest::prelude::*;
use serde_json::{Value, json};
use std::{
    io::{self, Cursor, Read, Seek, SeekFrom, Write},
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};

#[derive(Default)]
struct Fixture {
    replies: Vec<Vec<u8>>,
    writes: Arc<Mutex<Vec<u8>>>,
    connects: usize,
}
struct Connection {
    input: Cursor<Vec<u8>>,
    output: Arc<Mutex<Vec<u8>>>,
}
impl Read for Connection {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.input.read(out)
    }
}
impl Write for Connection {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Transport for Fixture {
    fn connect(&mut self, _: &Url, _: Limit, _: Arc<AtomicBool>) -> io::Result<Box<dyn Stream>> {
        let reply = self
            .replies
            .get(self.connects)
            .or(self.replies.last())
            .cloned()
            .unwrap_or_default();
        self.connects += 1;
        Ok(Box::new(Connection {
            input: Cursor::new(reply),
            output: self.writes.clone(),
        }))
    }
}
fn reply(status: u16, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!(
        "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes
}
fn client() -> Client {
    Client::new(
        "https://peer.test/kosync",
        Credentials::from_password("alice", "password").unwrap(),
        false,
    )
    .unwrap()
}
fn limit() -> Limit {
    Limit::Until(Instant::now() + Duration::from_secs(10))
}
fn cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
fn id() -> DocumentId {
    DocumentId::filename("book.epub").unwrap()
}

// ------------------------------------------------------------ the contract

fn plain(value: &Value, limit: usize, may_be_empty: bool) -> bool {
    value.as_str().is_some_and(|text| {
        (may_be_empty || !text.is_empty())
            && text.len() <= limit
            && !text.chars().any(char::is_control)
    })
}
/// docs/koreader-sync.md: the client "validates document identity, progress
/// fields and timestamps before returning a result".
fn acceptable(record: &serde_json::Map<String, Value>, document: &str) -> bool {
    let get = |key: &str| record.get(key).unwrap_or(&Value::Null);
    get("document").as_str() == Some(document)
        && plain(get("progress"), 8192, false)
        && plain(get("device"), 256, false)
        && (!record.contains_key("device_id") || plain(get("device_id"), 256, true))
        && get("percentage")
            .as_f64()
            .is_some_and(|p| (0.0..=1.0).contains(&p))
        && get("timestamp").as_u64().is_some_and(|t| t > 0)
}

// ------------------------------------------------------------ generators

fn string_field(limit: usize) -> impl Strategy<Value = Option<Value>> {
    prop_oneof![
        6 => "[a-zA-Z0-9/\\[\\]\\.() é]{1,40}".prop_map(|s| Some(Value::from(s))),
        1 => Just(None),
        1 => Just(Some(Value::Null)),
        1 => Just(Some(Value::from(""))),
        1 => Just(Some(Value::from(7))),
        1 => Just(Some(json!(["x"]))),
        1 => Just(Some(Value::from("line\nbreak"))),
        1 => Just(Some(Value::from("nul\u{0}"))),
        1 => Just(Some(Value::from("\u{85}"))),
        1 => Just(Some(Value::from("x".repeat(limit)))),
        1 => Just(Some(Value::from("x".repeat(limit + 1)))),
        1 => Just(Some(Value::from("é".repeat(limit / 2)))),
        1 => Just(Some(Value::from(format!("{}y", "é".repeat(limit / 2))))),
    ]
}
fn number(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}
fn percentage() -> impl Strategy<Value = Option<Value>> {
    prop_oneof![
        6 => (0.0..=1.0f64).prop_map(|p| Some(Value::from(p))),
        1 => Just(None),
        1 => Just(Some(Value::Null)),
        1 => prop::sample::select(vec![
            "0", "1", "0.0", "1.0", "-0.0", "1.0000001", "-0.0000001", "2", "-1", "1e308",
            "1e-320", "\"0.5\"", "true", "100",
        ])
        .prop_map(|text| Some(number(text))),
    ]
}
fn timestamp() -> impl Strategy<Value = Option<Value>> {
    prop_oneof![
        6 => (1..=u64::MAX).prop_map(|t| Some(Value::from(t))),
        1 => Just(None),
        1 => Just(Some(Value::Null)),
        2 => prop::sample::select(vec![
            "0", "1", "-1", "18446744073709551615", "18446744073709551616", "1.5",
            "\"1730000000\"", "true", "-0",
        ])
        .prop_map(|text| Some(number(text))),
    ]
}
fn document() -> impl Strategy<Value = Option<Value>> {
    prop_oneof![
        8 => Just(Some(Value::from(id().as_str()))),
        1 => Just(None),
        1 => Just(Some(Value::from(id().as_str().to_uppercase()))),
        1 => Just(Some(Value::from("0123456789abcdef0123456789abcdef"))),
        1 => Just(Some(Value::from(format!("{} ", id().as_str())))),
        1 => Just(Some(Value::from(7))),
    ]
}
fn record() -> impl Strategy<Value = serde_json::Map<String, Value>> {
    (
        document(),
        string_field(8192),
        percentage(),
        string_field(256),
        string_field(256),
        timestamp(),
        any::<bool>(),
    )
        .prop_map(
            |(document, progress, percentage, device, device_id, timestamp, extra)| {
                let mut map = serde_json::Map::new();
                for (key, value) in [
                    ("document", document),
                    ("progress", progress),
                    ("percentage", percentage),
                    ("device", device),
                    ("device_id", device_id),
                    ("timestamp", timestamp),
                ] {
                    if let Some(value) = value {
                        map.insert(key.into(), value);
                    }
                }
                if extra {
                    map.insert("unknown".into(), json!({"nested": [1, 2]}));
                }
                map
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 3000, ..ProptestConfig::default() })]

    #[test]
    fn a_progress_reply_is_accepted_exactly_when_the_contract_accepts_it(record in record()) {
        let body = Value::Object(record.clone()).to_string();
        prop_assume!(body.len() <= femto_kosync::MAX_RESPONSE_BYTES);
        let mut transport = Fixture { replies: vec![reply(200, body.as_bytes())], ..Default::default() };
        let result = client().pull(&mut transport, &id(), limit(), cancel());
        if record.is_empty() {
            prop_assert!(matches!(result, Ok(None)));
        } else if acceptable(&record, id().as_str()) {
            let remote = result.expect("an acceptable record").expect("a record");
            // What is returned is what was sent.
            prop_assert_eq!(remote.position.document.as_str(), record["document"].as_str().unwrap());
            prop_assert_eq!(remote.position.progress.as_str(), record["progress"].as_str().unwrap());
            prop_assert_eq!(remote.position.device.as_str(), record["device"].as_str().unwrap());
            // serde_json without `float_roundtrip` may be one unit in the
            // last place off; a percentage does not need better.
            prop_assert!((remote.position.percentage - record["percentage"].as_f64().unwrap()).abs() < 1e-12);
            prop_assert_eq!(
                remote.position.device_id.as_str(),
                record.get("device_id").map_or("", |v| v.as_str().unwrap())
            );
            prop_assert_eq!(remote.timestamp, record["timestamp"].as_u64().unwrap());
        } else {
            prop_assert!(result.is_err(), "accepted {body}");
        }
    }

    /// An upload is told it succeeded only by a receipt for its document
    /// with a time, or by a record read back.
    #[test]
    fn an_upload_succeeds_only_on_a_receipt_or_a_record_read_back(
        receipt in record(),
        stored in record(),
    ) {
        let progress = Progress {
            document: id(),
            progress: "/body/DocFragment[1]/body/p/text().5".into(),
            percentage: 0.25,
            device: "Femto".into(),
            device_id: "fixture-device".into(),
        };
        let first = Value::Object(receipt.clone()).to_string();
        let second = Value::Object(stored.clone()).to_string();
        prop_assume!(first.len().max(second.len()) <= femto_kosync::MAX_RESPONSE_BYTES);
        let mut transport = Fixture {
            replies: vec![reply(200, first.as_bytes()), reply(200, second.as_bytes())],
            ..Default::default()
        };
        let result = client().push(&mut transport, &progress, limit(), cancel());
        // A receipt is a document and a whole-number time.
        let is_receipt = receipt.get("document").is_some_and(|d| {
            d.as_str().is_some_and(|d| d.len() == 32 && d.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        }) && receipt.get("timestamp").is_some_and(Value::is_u64);
        let expected = if is_receipt {
            (receipt["document"].as_str() == Some(id().as_str()) && receipt["timestamp"].as_u64() != Some(0))
                .then(|| receipt["timestamp"].as_u64().unwrap())
        } else {
            (!stored.is_empty() && acceptable(&stored, id().as_str()))
                .then(|| stored["timestamp"].as_u64().unwrap())
        };
        prop_assert_eq!(result.as_ref().ok().map(|r| r.timestamp), expected, "{} then {}: {:?}", first, second, result);
        prop_assert_eq!(transport.connects, if is_receipt { 1 } else { 2 });
    }

    /// Whatever the fields hold, an upload either is refused before anything
    /// is sent or sends exactly what it was given.
    #[test]
    fn an_upload_is_sent_whole_or_not_at_all(
        progress in prop_oneof![".{0,40}", Just("x".repeat(8192)), Just("x".repeat(8193))],
        device in prop_oneof![".{0,20}", Just("x".repeat(257))],
        device_id in prop_oneof![".{0,20}", Just("x".repeat(257))],
        percentage in prop_oneof![
            (-0.5..1.5f64),
            Just(0.0), Just(1.0), Just(f64::NAN), Just(f64::INFINITY), Just(-0.0),
        ],
    ) {
        let sent = Progress { document: id(), progress, percentage, device, device_id };
        let fine = !sent.progress.is_empty() && sent.progress.len() <= 8192
            && !sent.device.is_empty() && sent.device.len() <= 256 && sent.device_id.len() <= 256
            && ![&sent.progress, &sent.device, &sent.device_id].iter().any(|s| s.chars().any(char::is_control))
            && (0.0..=1.0).contains(&sent.percentage);
        let receipt = json!({"document": id().as_str(), "timestamp": 9}).to_string();
        let mut transport = Fixture { replies: vec![reply(200, receipt.as_bytes())], ..Default::default() };
        let result = client().push(&mut transport, &sent, limit(), cancel());
        prop_assert_eq!(result.is_ok(), fine, "{:?}", sent);
        if !fine {
            prop_assert_eq!(transport.connects, 0);
            return Ok(());
        }
        let bytes = transport.writes.lock().unwrap().clone();
        let split = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = std::str::from_utf8(&bytes[..split]).unwrap();
        let body = &bytes[split + 4..];
        let length = format!("Content-Length: {}", body.len());
        prop_assert!(head.contains(&length));
        prop_assert_eq!(head.lines().count(), 9, "{}", head);
        // Decoding here may be one unit in the last place off.
        let mut received: Progress = serde_json::from_slice(body).unwrap();
        prop_assert!((received.percentage - sent.percentage).abs() < 1e-12);
        received.percentage = sent.percentage;
        prop_assert_eq!(received, sent);
    }

    /// No reply, however broken, panics or is taken for a result.
    #[test]
    fn broken_replies_are_errors(
        status in prop::sample::select(vec![200u16, 201, 204, 301, 400, 401, 404, 500]),
        body in prop_oneof![
            prop::collection::vec(any::<u8>(), 0..200),
            "[\\[\\]{}\":,0-9a-z ]{0,80}".prop_map(String::into_bytes),
        ],
        cut in any::<prop::sample::Index>(),
    ) {
        let whole = reply(status, &body);
        let cut = cut.index(whole.len() + 1);
        for bytes in [whole.clone(), whole[..cut].to_vec()] {
            let mut transport = Fixture { replies: vec![bytes.clone()], ..Default::default() };
            let pulled = client().pull(&mut transport, &id(), limit(), cancel());
            let object = serde_json::from_slice::<Value>(&body).ok().filter(|_| bytes.len() == whole.len());
            match &pulled {
                Ok(None) => prop_assert!(
                    (status == 404 && bytes.len() >= 12) || (status == 200 && object == Some(json!({}))),
                    "no record from {:?}", String::from_utf8_lossy(&bytes)
                ),
                Ok(Some(_)) => prop_assert!(false, "a record from {:?}", String::from_utf8_lossy(&bytes)),
                Err(_) => (),
            }
            let mut transport = Fixture { replies: vec![bytes.clone()], ..Default::default() };
            let progress = Progress { document: id(), progress: "1".into(), percentage: 0.0, device: "d".into(), device_id: "i".into() };
            prop_assert!(client().push(&mut transport, &progress, limit(), cancel()).is_err());
            let mut transport = Fixture { replies: vec![bytes.clone()], ..Default::default() };
            prop_assert!(client().register(&mut transport, limit(), cancel()).is_err());
        }
    }

    /// KOReader's `util.partialMD5`: 1 KiB at offset zero and at 1024 << 2i
    /// for i in 0..=10, stopping at the first sample past the end.
    #[test]
    fn a_sampled_identity_follows_koreaders_sampling(
        length in prop_oneof![
            0..5000usize,
            prop::sample::select(vec![1023usize, 1024, 1025, 2047, 2048, 4095, 4096, 4097, 5120, 16383, 16384, 17408, 65536, 65537, 66560, 70000]),
        ],
        seed in any::<u8>(),
        chunk in 1..2000usize,
        start in 0..100u64,
    ) {
        let bytes: Vec<u8> = (0..length).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) ^ (i >> 8) as u8).collect();
        let mut hash = Md5::new();
        for offset in [0usize, 1024, 4096, 16384, 65536, 262144] {
            if offset >= bytes.len() {
                break;
            }
            hash.update(&bytes[offset..bytes.len().min(offset + 1024)]);
        }
        let expected = format!("{:x}", hash.finalize());
        struct Short(Cursor<Vec<u8>>, usize);
        impl Read for Short {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                let n = out.len().min(self.1);
                self.0.read(&mut out[..n])
            }
        }
        impl Seek for Short {
            fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
                self.0.seek(to)
            }
        }
        let mut source = Short(Cursor::new(bytes), chunk);
        let start = start.min(length as u64);
        source.seek(SeekFrom::Start(start)).unwrap();
        let id = DocumentId::sampled(&mut source).unwrap();
        prop_assert_eq!(id.as_str(), expected.as_str());
        prop_assert_eq!(source.stream_position().unwrap(), start);
    }

    /// Settings built from arbitrary values never panic, and never show a
    /// secret.
    #[test]
    fn settings_never_panic_or_show_a_secret(
        secret in "[A-Z]{12}",
        username in prop_oneof![Just("alice".to_string()), ".{0,12}"],
        server in prop_oneof![
            Just("https://sync.test/".to_string()),
            Just("http://sync.test".to_string()),
            Just("https://user:pw@sync.test/".to_string()),
            Just("ftp://sync.test/".to_string()),
            ".{0,20}",
        ],
        device_id in prop_oneof![Just("id".to_string()), ".{0,12}"],
        as_key in any::<bool>(),
        both in any::<bool>(),
        allow_http in any::<bool>(),
        raw in prop::collection::vec(any::<u8>(), 0..60),
    ) {
        let _ = Settings::from_json(&raw);
        let key = format!("{:x}", Md5::digest(secret.as_bytes()));
        let mut value = json!({"server": server, "username": username, "device_id": device_id, "allow_http": allow_http});
        if as_key || both {
            value["userkey"] = key.clone().into();
        }
        if !as_key || both {
            value["password"] = secret.clone().into();
        }
        let shown = match Settings::from_json(value.to_string().as_bytes()) {
            Ok(settings) => {
                prop_assert!(!both);
                prop_assert!(server.starts_with("https://") || (allow_http && server.starts_with("http://")), "{}", server);
                format!("{settings:?}")
            }
            Err(error) => format!("{error} {error:?}"),
        };
        prop_assert!(!shown.contains(&secret) && !shown.contains(&key), "{}", shown);
    }
}
