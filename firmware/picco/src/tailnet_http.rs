//! Catalog HTTP over the reader's selected tailnet runtime generation.
use femto_catalog::http::{self, Stream, Url};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

pub struct Transport(pub Option<crate::tailnet_menu::Driver>);
impl http::Transport for Transport {
    fn connect(
        &mut self,
        url: &Url,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn Stream>> {
        if cancelled.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let host = url
            .host_str()
            .filter(|_| url.domain().is_some())
            .ok_or(io::ErrorKind::InvalidInput)?;
        let port = url
            .port_or_known_default()
            .filter(|port| *port != 0)
            .ok_or(io::ErrorKind::InvalidInput)?;
        if url.scheme() == "https" {
            crate::clock::require_ready()?;
        }
        let driver = self.0.clone().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "Connect the reader tailnet first",
            )
        })?;
        let socket = femto_microlink::application_tcp::Stream::connect(
            driver, host, port, deadline, cancelled,
        )?;
        if url.scheme() == "https" {
            Ok(Box::new(crate::tailnet_tls::Tls::connect(socket, host)?))
        } else {
            Ok(Box::new(socket))
        }
    }
}
