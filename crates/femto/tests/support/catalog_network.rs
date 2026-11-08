use femto_catalog::http::{self, Stream, Url};
use std::{
    io::{self, Cursor, Read, Write},
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};
pub struct Transport;
struct Reply {
    input: Cursor<Vec<u8>>,
    output: Vec<u8>,
    authenticated: bool,
}
impl Read for Reply {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        let request = std::str::from_utf8(&self.output).unwrap();
        assert_eq!(
            request.contains("Authorization: Basic "),
            self.authenticated
        );
        self.input.read(b)
    }
}
impl Write for Reply {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl http::Transport for Transport {
    fn connect(
        &mut self,
        url: &Url,
        _: Instant,
        _: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn Stream>> {
        if url.path() == "/fail" {
            return Err(io::Error::other("injected failure"));
        }
        let mut value = serde_json::json!({
            "metadata": {"title": url.path()},
            "links": [{"rel":"self", "href":url.as_str()}, {"rel":"next", "href":"/next", "type":"application/opds+json"}],
            "publications": [{"metadata":{"title":"A book", "author":[{"name":"An author"}]}, "links":[{"rel":"http://opds-spec.org/acquisition/open-access", "href":"/book.epub", "type":"application/epub+zip"}]}]
        });
        if url.path() == "/badfeed" {
            value["publications"][0]["links"][0]["href"] = serde_json::json!("/bad.epub");
        }
        if url.path() == "/searchable" {
            value["links"].as_array_mut().unwrap().push(serde_json::json!({"rel":"search", "href":"/results{?query}", "templated":true, "type":"application/opds+json"}));
        }
        let (feed, media) = if url.path() == "/book.epub" {
            (
                include_bytes!("../../../../tests/fixtures/sample.epub").to_vec(),
                "application/epub+zip",
            )
        } else if url.path() == "/bad.epub" {
            (b"invalid epub".to_vec(), "application/epub+zip")
        } else {
            (serde_json::to_vec(&value).unwrap(), "application/opds+json")
        };
        let mut reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {media}\r\nContent-Length: {}\r\n\r\n",
            feed.len()
        )
        .into_bytes();
        reply.extend(feed);
        Ok(Box::new(Reply {
            input: Cursor::new(reply),
            output: Vec::new(),
            authenticated: url.host_str() == Some("auth.example"),
        }))
    }
}
