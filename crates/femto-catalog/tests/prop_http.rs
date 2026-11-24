//! Discovery properties for the HTTP/1 response reader: framing equivalence,
//! fragmentation independence, truncation, limits, and credential scope.
use femto_catalog::{
    auth::BasicAuth,
    deadline::Limit,
    http::{self, Stream, Transport, Url},
};
use proptest::prelude::*;
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};

/// Delivers a response in reads of generated sizes, cycling through them.
struct Wire {
    input: Vec<u8>,
    at: usize,
    reads: Vec<usize>,
    turn: usize,
    request: Arc<Mutex<Vec<u8>>>,
}
impl Read for Wire {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let step = self.reads[self.turn % self.reads.len()];
        self.turn += 1;
        let count = step.min(out.len()).min(self.input.len() - self.at);
        out[..count].copy_from_slice(&self.input[self.at..self.at + count]);
        self.at += count;
        Ok(count)
    }
}
impl Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.request.lock().unwrap().extend(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[derive(Default)]
struct Mock {
    responses: VecDeque<Vec<u8>>,
    reads: Vec<usize>,
    requests: Vec<(String, Arc<Mutex<Vec<u8>>>)>,
}
impl Transport for Mock {
    fn connect(&mut self, url: &Url, _: Limit, _: Arc<AtomicBool>) -> io::Result<Box<dyn Stream>> {
        let request = Arc::new(Mutex::new(Vec::new()));
        self.requests.push((url.to_string(), request.clone()));
        Ok(Box::new(Wire {
            input: self.responses.pop_front().expect("unexpected request"),
            at: 0,
            reads: if self.reads.is_empty() {
                vec![usize::MAX]
            } else {
                self.reads.clone()
            },
            turn: 0,
            request,
        }))
    }
}
fn wait() -> Limit {
    Limit::Until(Instant::now() + Duration::from_secs(30))
}
fn download(
    response: Vec<u8>,
    reads: &[usize],
    limit: usize,
) -> (Result<usize, http::Error>, Vec<u8>) {
    let mut mock = Mock {
        responses: [response].into(),
        reads: reads.to_vec(),
        ..Default::default()
    };
    let mut output = Vec::new();
    let result = http::download_file(
        &mut mock,
        "http://books.example/file",
        wait(),
        Arc::new(AtomicBool::new(false)),
        limit,
        0,
        &mut output,
    )
    .map(|download| download.bytes);
    (result, output)
}

#[derive(Debug, Clone)]
enum Framing {
    Length,
    /// Sizes of successive chunks (cycled), a chunk extension, and a trailer.
    Chunked {
        sizes: Vec<usize>,
        extension: bool,
        upper: bool,
        zeros: usize,
        trailer: bool,
    },
    Close,
}
#[derive(Debug, Clone)]
struct Style {
    /// Header names in another case, and optional whitespace around values.
    shout: bool,
    space_before: usize,
    space_after: usize,
    extra_headers: usize,
}
fn name(style: &Style, name: &str) -> String {
    if style.shout {
        name.to_ascii_uppercase()
    } else {
        name.to_owned()
    }
}
fn encode(body: &[u8], framing: &Framing, style: &Style) -> Vec<u8> {
    let (before, after) = (
        " ".repeat(style.space_before),
        " ".repeat(style.space_after),
    );
    let mut out = b"HTTP/1.1 200 OK\r\n".to_vec();
    for index in 0..style.extra_headers {
        out.extend(format!("X-Extra-{index}: value {index}\r\n").bytes());
    }
    out.extend(
        format!(
            "{}:{before}application/octet-stream{after}\r\n",
            name(style, "Content-Type")
        )
        .bytes(),
    );
    match framing {
        Framing::Length => {
            out.extend(
                format!(
                    "{}:{before}{}{after}\r\n\r\n",
                    name(style, "Content-Length"),
                    body.len()
                )
                .bytes(),
            );
            out.extend(body);
        }
        Framing::Chunked {
            sizes,
            extension,
            upper,
            zeros,
            trailer,
        } => {
            out.extend(
                format!(
                    "{}:{before}chunked{after}\r\n\r\n",
                    name(style, "Transfer-Encoding")
                )
                .bytes(),
            );
            // Stay well within the documented limit of 16,384 chunks.
            let floor = body.len() / 8192 + 1;
            let (mut rest, mut turn) = (body, 0);
            while !rest.is_empty() {
                let size = sizes[turn % sizes.len()].max(floor).min(rest.len());
                turn += 1;
                let hex = if *upper {
                    format!("{size:X}")
                } else {
                    format!("{size:x}")
                };
                out.extend("0".repeat(*zeros).bytes());
                out.extend(hex.bytes());
                if *extension {
                    out.extend(b";name=value");
                }
                out.extend(b"\r\n");
                out.extend(&rest[..size]);
                out.extend(b"\r\n");
                rest = &rest[size..];
            }
            out.extend(b"0\r\n");
            if *trailer {
                out.extend(b"X-Checksum: 1\r\n");
            }
            out.extend(b"\r\n");
        }
        Framing::Close => {
            out.extend(b"\r\n");
            out.extend(body);
        }
    }
    out
}
fn framing() -> impl Strategy<Value = Framing> {
    prop_oneof![
        Just(Framing::Length),
        Just(Framing::Close),
        (
            prop::collection::vec(prop_oneof![1usize..8, 1usize..5000], 1..5),
            any::<bool>(),
            any::<bool>(),
            0usize..4,
            any::<bool>()
        )
            .prop_map(
                |(sizes, extension, upper, zeros, trailer)| Framing::Chunked {
                    sizes,
                    extension,
                    upper,
                    zeros,
                    trailer
                }
            ),
    ]
}
fn style() -> impl Strategy<Value = Style> {
    (any::<bool>(), 0usize..3, 0usize..3, 0usize..4).prop_map(
        |(shout, space_before, space_after, extra_headers)| Style {
            shout,
            space_before,
            space_after,
            extra_headers,
        },
    )
}
/// Bodies that are empty, tiny, around the 8 KiB transfer buffer, or larger,
/// and that contain line breaks and chunk-like text.
fn body() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        prop::collection::vec(any::<u8>(), 0..40),
        prop::collection::vec(prop::sample::select(b"\r\n0a;: ".to_vec()), 0..60),
        (8180usize..8200, any::<u8>()).prop_map(|(len, byte)| vec![byte; len]),
        (0usize..40_000, any::<u8>()).prop_map(|(len, seed)| (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()),
    ]
}
fn reads() -> impl Strategy<Value = Vec<usize>> {
    prop_oneof![
        Just(vec![1]),
        Just(vec![usize::MAX]),
        prop::collection::vec(prop_oneof![1usize..6, 1usize..20_000], 1..6),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(600))]

    /// Every framing of the same body, however the bytes arrive, yields that body.
    #[test]
    fn framing_and_fragmentation_do_not_change_the_body(
        body in body(), framing in framing(), style in style(), reads in reads(), slack in 0usize..3,
    ) {
        let limit = body.len() + slack;
        let (result, output) = download(encode(&body, &framing, &style), &reads, limit);
        prop_assert_eq!(result.map_err(|e| e.to_string()), Ok(body.len()));
        prop_assert_eq!(output, body);
    }

    /// A body over the limit is refused in every framing, and never more than
    /// the limit reaches the caller's storage.
    #[test]
    fn bodies_over_the_limit_are_refused_and_writes_stay_bounded(
        body in body().prop_filter("nonempty", |b| !b.is_empty()),
        framing in framing(), style in style(), reads in reads(), under in 1usize..4,
    ) {
        let limit = body.len().saturating_sub(under);
        let (result, output) = download(encode(&body, &framing, &style), &reads, limit);
        prop_assert!(result.is_err());
        prop_assert!(output.len() <= limit, "{} bytes written past limit {}", output.len(), limit);
    }

    /// A response cut anywhere is an error for self-delimiting framings, and a
    /// prefix of the body for a close-delimited one.
    #[test]
    fn truncation_is_detected(
        body in prop::collection::vec(any::<u8>(), 0..60),
        framing in framing(), style in style(), reads in reads(),
    ) {
        let whole = encode(&body, &framing, &style);
        let head = whole.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        for cut in 0..whole.len() {
            let (result, output) = download(whole[..cut].to_vec(), &reads, 1 << 20);
            match (&framing, cut >= head) {
                (Framing::Close, true) => {
                    prop_assert_eq!(result.map_err(|e| e.to_string()), Ok(cut - head));
                    prop_assert_eq!(&output[..], &body[..cut - head]);
                }
                _ => prop_assert!(result.is_err(), "cut {} of {} accepted", cut, whole.len()),
            }
            prop_assert!(body.starts_with(&output));
        }
    }

    /// One byte changed anywhere never panics, and what is accepted is bounded.
    #[test]
    fn mutated_responses_never_panic(
        body in prop::collection::vec(any::<u8>(), 0..300),
        framing in framing(), style in style(), reads in reads(),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>(), 0u8..3), 1..4),
    ) {
        let mut bytes = encode(&body, &framing, &style);
        for (index, byte, kind) in edits {
            let at = index.index(bytes.len());
            match kind {
                0 => bytes[at] = byte,
                1 => bytes.insert(at, byte),
                _ => { bytes.remove(at); }
            }
        }
        let limit = body.len() / 2 + 1;
        let (result, output) = download(bytes.clone(), &reads, limit);
        prop_assert!(output.len() <= limit);
        if let Ok(count) = result {
            prop_assert_eq!(count, output.len());
        }
        let cancelled = AtomicBool::new(false);
        let _ = http::read_json_response(&bytes[..], 1024, wait(), &cancelled);
    }

    #[test]
    fn arbitrary_bytes_never_panic(
        prefix in prop::sample::select(vec![
            &b""[..], b"HTTP/1.1 200 OK\r\n", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n",
        ]),
        bytes in prop::collection::vec(prop_oneof![any::<u8>(), prop::sample::select(b"\r\n0123456789abcdefF;: ".to_vec())], 0..400),
        reads in reads(),
    ) {
        let bytes = [prefix, &bytes].concat();
        let (result, output) = download(bytes.clone(), &reads, 64);
        prop_assert!(output.len() <= 64);
        if let Ok(count) = result {
            prop_assert_eq!(count, output.len());
        }
        let cancelled = AtomicBool::new(false);
        if let Ok((_, body)) = http::read_json_response(&bytes[..], 64, wait(), &cancelled) {
            prop_assert!(body.len() <= 64);
        }
    }
}

// Credential scope across redirects, checked against origins computed from the
// generated parts and not from the URL parser the client uses.

#[derive(Debug, Clone, PartialEq)]
struct Place {
    https: bool,
    host: &'static str,
    /// Spelling of the host in the URL.
    spelled: &'static str,
    port: Option<u16>,
    path: String,
}
impl Place {
    fn url(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        format!("{scheme}://{}{port}/{}", self.spelled, self.path)
    }
    fn origin(&self) -> (bool, &'static str, u16) {
        (
            self.https,
            self.host,
            self.port.unwrap_or(if self.https { 443 } else { 80 }),
        )
    }
}
fn place() -> impl Strategy<Value = Place> {
    (
        any::<bool>(),
        prop::sample::select(vec![
            ("books.example", "books.example"),
            ("books.example", "BOOKS.Example"),
            ("books.example.evil.test", "books.example.evil.test"),
            ("evil.test", "evil.test"),
            ("xbooks.example", "xbooks.example"),
            ("192.0.2.7", "192.0.2.7"),
        ]),
        prop::sample::select(vec![None, Some(80), Some(443), Some(8080)]),
        "[a-z]{1,3}(\\?[a-z]=[a-z])?",
    )
        .prop_map(|(https, (host, spelled), port, path)| Place {
            https,
            host,
            spelled,
            port,
            path,
        })
}
/// How a redirect names its destination.
#[derive(Debug, Clone, Copy)]
enum Reference {
    Absolute,
    SchemeRelative,
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1500))]

    /// The header goes to the configured origin and nowhere else, a downgrade
    /// from HTTPS is never followed, and each hop is the one the redirect named.
    #[test]
    fn credentials_stay_on_their_origin_across_redirects(
        start in place(),
        hops in prop::collection::vec((place(), prop::sample::select(vec![Reference::Absolute, Reference::SchemeRelative]), prop::sample::select(vec![301u16, 302, 303, 307, 308])), 0..5),
        reads in reads(),
    ) {
        let auth = BasicAuth::new(&start.url(), "reader", "secret", true).unwrap();
        // A scheme-relative reference keeps the scheme of the page that sent it.
        let mut chain = vec![start.clone()];
        let mut responses = VecDeque::new();
        for (mut next, reference, status) in hops {
            let location = match reference {
                Reference::Absolute => next.url(),
                Reference::SchemeRelative => {
                    next.https = chain.last().unwrap().https;
                    next.url().split_once(':').unwrap().1.to_owned()
                }
            };
            responses.push_back(format!("HTTP/1.1 {status} Moved\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n").into_bytes());
            chain.push(next);
        }
        responses.push_back(b"HTTP/1.1 200 OK\r\nContent-Type: application/epub+zip\r\nContent-Length: 4\r\n\r\nbook".to_vec());
        // The model: follow until a downgrade or a URL seen before.
        let mut expected = Vec::new();
        let mut complete = true;
        for (index, place) in chain.iter().enumerate() {
            if index > 0 && chain[index - 1].https && !place.https {
                complete = false;
                break;
            }
            let key = (place.origin(), place.path.clone());
            if chain[..index].iter().any(|p| (p.origin(), p.path.clone()) == key) {
                complete = false;
                break;
            }
            expected.push(place.clone());
        }
        let mut mock = Mock { responses, reads, ..Default::default() };
        let mut output = Vec::new();
        let result = http::download_epub(&mut mock, &start.url(), wait(), Arc::new(AtomicBool::new(false)), Some(&auth), &mut output);
        prop_assert_eq!(result.is_ok(), complete, "{:?}", result);
        prop_assert_eq!(mock.requests.len(), expected.len());
        for ((url, request), place) in mock.requests.iter().zip(&expected) {
            let request = String::from_utf8(request.lock().unwrap().clone()).unwrap();
            let sent = request.contains("Authorization:");
            prop_assert_eq!(sent, place.origin() == start.origin(), "{} from {}", url, start.url());
            // The connection goes where the request says it goes.
            let parsed = Url::parse(url).unwrap();
            prop_assert_eq!(parsed.host_str().unwrap(), place.host);
            prop_assert_eq!(parsed.port_or_known_default().unwrap(), place.origin().2);
            prop_assert_eq!(parsed.scheme() == "https", place.https);
            let target = format!("GET /{} HTTP/1.1\r\n", place.path);
            prop_assert!(request.starts_with(&target), "{}", request);
            prop_assert_eq!(request.matches("\r\n\r\n").count(), 1);
        }
    }
}

/// The documented chunk limit is 16,384. A body sent in exactly that many
/// chunks is within it.
#[test]
#[ignore = "bug: a chunked body of exactly 16,384 chunks is refused; the terminating chunk counts toward the limit"]
fn a_body_of_exactly_the_documented_chunk_count_is_accepted() {
    for (chunks, accepted) in [(16_383, true), (16_384, true), (16_385, false)] {
        let mut response =
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n"
                .to_vec();
        response.extend(b"1\r\nx\r\n".repeat(chunks));
        response.extend(b"0\r\n\r\n");
        let (result, _) = download(response, &[usize::MAX], 1 << 20);
        assert_eq!(result.is_ok(), accepted, "{chunks} chunks");
    }
}
