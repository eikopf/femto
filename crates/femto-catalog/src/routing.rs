//! Exact-origin transport selection, including redirects, without fallback.
use crate::{
    deadline::Limit,
    http::{Stream, Transport, Url},
};
use std::{
    io,
    sync::{Arc, atomic::AtomicBool},
};

pub struct Routed<L, T> {
    lan: L,
    tailnet: T,
    origins: Vec<url::Origin>,
}
impl<L, T> Routed<L, T> {
    /// At most sixteen HTTP(S) origins may select the tailnet transport.
    pub fn new(lan: L, tailnet: T, origins: &[&str]) -> io::Result<Self> {
        if origins.len() > 16 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut normalized = Vec::new();
        for text in origins {
            let url = Url::parse(text).map_err(|_| io::ErrorKind::InvalidInput)?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host().is_none()
                || url.port_or_known_default() == Some(0)
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
                || url.query().is_some()
                || url.path() != "/"
            {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            let origin = url.origin();
            if !normalized.contains(&origin) {
                normalized.push(origin);
            }
        }
        Ok(Self {
            lan,
            tailnet,
            origins: normalized,
        })
    }
}
impl<L: Transport, T: Transport> Transport for Routed<L, T> {
    fn connect(
        &mut self,
        url: &Url,
        limit: Limit,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn Stream>> {
        if self.origins.contains(&url.origin()) {
            self.tailnet.connect(url, limit, cancelled)
        } else {
            self.lan.connect(url, limit, cancelled)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        collections::VecDeque,
        io::{Cursor, Read, Write},
        rc::Rc,
        time::{Duration, Instant},
    };
    struct Wire(Cursor<Vec<u8>>);
    impl Read for Wire {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.0.read(out)
        }
    }
    impl Write for Wire {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    struct Fixture {
        label: &'static str,
        calls: Rc<RefCell<Vec<String>>>,
        replies: VecDeque<io::Result<Vec<u8>>>,
    }
    impl Transport for Fixture {
        fn connect(
            &mut self,
            url: &Url,
            _: Limit,
            _: Arc<AtomicBool>,
        ) -> io::Result<Box<dyn Stream>> {
            self.calls
                .borrow_mut()
                .push(format!("{} {}", self.label, url));
            Ok(Box::new(Wire(Cursor::new(
                self.replies.pop_front().expect("unexpected dial")?,
            ))))
        }
    }
    #[test]
    fn redirects_select_each_origin_and_tailnet_errors_never_fallback() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let body = br#"{"metadata":{"title":"Tailnet books"},"links":[{"rel":"self","href":"/feed","type":"application/opds+json"}],"publications":[]}"#;
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/opds+json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        let lan = Fixture { label: "lan", calls: calls.clone(), replies: [Ok(b"HTTP/1.1 302 Found\r\nLocation: http://books.example/feed\r\nContent-Length: 0\r\n\r\n".to_vec())].into() };
        let tailnet = Fixture {
            label: "tailnet",
            calls: calls.clone(),
            replies: [Ok(response), Err(io::ErrorKind::ConnectionRefused.into())].into(),
        };
        let mut router = Routed::new(lan, tailnet, &["http://BOOKS.example:80"]).unwrap();
        let limit = Limit::Until(Instant::now() + Duration::from_secs(2));
        let cancelled = Arc::new(AtomicBool::new(false));
        let catalog = crate::http::fetch(
            &mut router,
            "http://public.example/start",
            limit,
            cancelled.clone(),
        )
        .unwrap();
        assert_eq!(catalog.title, "Tailnet books");
        assert_eq!(catalog.source, "http://books.example/feed");
        assert_eq!(
            router
                .connect(
                    &Url::parse("http://books.example/fail").unwrap(),
                    limit,
                    cancelled
                )
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::ConnectionRefused
        );
        assert_eq!(
            &*calls.borrow(),
            &[
                "lan http://public.example/start",
                "tailnet http://books.example/feed",
                "tailnet http://books.example/fail"
            ]
        );
    }
    #[test]
    fn origin_configuration_is_bounded_and_rejects_paths_or_credentials() {
        for invalid in [
            "ftp://books.example",
            "http://user@books.example",
            "http://books.example/feed",
            "http://books.example/?q=1",
            "http://books.example/#a",
            "http://books.example:0",
        ] {
            assert!(Routed::new((), (), &[invalid]).is_err(), "{invalid}");
        }
        assert!(Routed::new((), (), &["http://books.example"; 17]).is_err());
        let router = Routed::new(
            (),
            (),
            &[
                "http://BOOKS.example:80",
                "http://books.example/",
                "https://books.example/",
            ],
        )
        .unwrap();
        assert_eq!(router.origins.len(), 2);
    }
}
