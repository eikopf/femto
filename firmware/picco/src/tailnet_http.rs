//! Catalog HTTP over the reader's selected tailnet runtime generation.
use crate::tailnet_menu::Session;
use femto_catalog::{
    deadline::Limit,
    http::{self, Stream, Url},
};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub struct Transport(pub crate::tailnet_menu::Lookup);
impl http::Transport for Transport {
    fn connect(
        &mut self,
        url: &Url,
        limit: Limit,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn Stream>> {
        if cancelled.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if limit.expired() {
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
        let until = limit.wait();
        let driver = loop {
            match (self.0)() {
                Session::Open(driver) => break driver,
                Session::Absent => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "Connect the reader tailnet first",
                    ));
                }
                Session::Awaited if cancelled.load(Ordering::Acquire) => {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                Session::Awaited if Instant::now() >= until => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "The clock is not set yet",
                    ));
                }
                Session::Awaited => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        let limit = match limit {
            Limit::Until(deadline) => femto_microlink::application_tcp::Limit::Until(deadline),
            Limit::Stall(limit) => femto_microlink::application_tcp::Limit::Stall(limit),
        };
        #[cfg(target_os = "espidf")]
        let started = std::time::Instant::now();
        let socket = femto_microlink::application_tcp::Stream::connect(
            driver, host, port, limit, cancelled,
        )?;
        // Includes joining the tailnet when the session was not yet up.
        #[cfg(target_os = "espidf")]
        log::info!(
            "Tailnet connection to {host}: {} ms",
            started.elapsed().as_millis()
        );
        if url.scheme() == "https" {
            Ok(Box::new(crate::tailnet_tls::Tls::connect(socket, host)?))
        } else {
            Ok(Box::new(socket))
        }
    }
}
