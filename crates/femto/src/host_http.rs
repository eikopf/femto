use femto_catalog::{
    deadline::Limit,
    http::{Stream, Transport, Url},
};
use femto_microlink::{derp::DeadlineIo, host_tls::TlsConnector, relay::Certificate};
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub struct HostTransport {
    pub endpoint: Option<(url::Origin, SocketAddr)>,
}
impl Transport for HostTransport {
    fn connect(
        &mut self,
        url: &Url,
        limit: Limit,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn Stream>> {
        remaining(limit.wait(), &cancelled)?;
        // Platform DNS is synchronous; the socket operations below remain bounded.
        let host = match url
            .host()
            .ok_or_else(|| io::Error::other("missing catalog host"))?
        {
            url::Host::Domain(name) => name.to_owned(),
            url::Host::Ipv4(ip) => ip.to_string(),
            url::Host::Ipv6(ip) => ip.to_string(),
        };
        let addresses: Vec<_> = match &self.endpoint {
            Some((origin, address)) if *origin == url.origin() => vec![*address],
            _ => (host.as_str(), url.port_or_known_default().unwrap())
                .to_socket_addrs()?
                .take(8)
                .collect(),
        };
        let mut last = io::Error::other("no catalog address available");
        for address in addresses {
            let remaining = remaining(limit.wait(), &cancelled)?;
            if url.scheme() == "https" {
                match TlsConnector::mozilla(remaining.min(Duration::from_secs(5)))?.connect(
                    address,
                    &host,
                    &Certificate::Name(host.clone()),
                    cancelled.clone(),
                ) {
                    Ok(stream) => return Ok(Box::new(Paced { stream, limit })),
                    Err(error) => last = error,
                }
            } else {
                match TcpStream::connect_timeout(&address, remaining.min(Duration::from_secs(5))) {
                    Ok(tcp) => {
                        return Ok(Box::new(Plain {
                            tcp,
                            limit,
                            cancelled,
                        }));
                    }
                    Err(error) => last = error,
                }
            }
        }
        Err(last)
    }
}
pub(crate) fn remaining(deadline: Instant, cancelled: &AtomicBool) -> io::Result<Duration> {
    if cancelled.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "catalog cancelled",
        ));
    }
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "catalog deadline"))
}
/// Gives each read and write of a deadline stream the limit's full wait.
struct Paced<S> {
    stream: S,
    limit: Limit,
}
impl<S: Read + DeadlineIo> Read for Paced<S> {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.stream.set_deadline(self.limit.wait());
        self.stream.read(b)
    }
}
impl<S: Write + DeadlineIo> Write for Paced<S> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.stream.set_deadline(self.limit.wait());
        self.stream.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.set_deadline(self.limit.wait());
        self.stream.flush()
    }
}
struct Plain {
    tcp: TcpStream,
    limit: Limit,
    cancelled: Arc<AtomicBool>,
}
impl Plain {
    fn operation<T>(
        &mut self,
        mut operation: impl FnMut(&mut TcpStream) -> io::Result<T>,
    ) -> io::Result<T> {
        let until = self.limit.wait();
        loop {
            let timeout = remaining(until, &self.cancelled)?.min(Duration::from_millis(100));
            self.tcp.set_read_timeout(Some(timeout))?;
            self.tcp.set_write_timeout(Some(timeout))?;
            match operation(&mut self.tcp) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
}
impl Read for Plain {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.operation(|s| s.read(b))
    }
}
impl Write for Plain {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.operation(|s| s.write(b))
    }
    fn flush(&mut self) -> io::Result<()> {
        self.operation(Write::flush)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stalled_plain_socket_observes_cancellation_and_its_limit() {
        use std::net::TcpListener;
        for cancel in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let tcp = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (_server, _) = listener.accept().unwrap();
            let flag = Arc::new(AtomicBool::new(false));
            let mut stream = Plain {
                tcp,
                limit: Limit::Stall(Duration::from_millis(if cancel { 2000 } else { 40 })),
                cancelled: flag.clone(),
            };
            let worker = cancel.then(|| {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(20));
                    flag.store(true, Ordering::Release);
                })
            });
            let error = stream.read_exact(&mut [0]).unwrap_err();
            assert_eq!(
                error.kind(),
                if cancel {
                    io::ErrorKind::ConnectionAborted
                } else {
                    io::ErrorKind::TimedOut
                }
            );
            if let Some(worker) = worker {
                worker.join().unwrap();
            }
        }
    }
}
