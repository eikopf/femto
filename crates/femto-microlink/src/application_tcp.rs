//! Blocking application I/O over a caller-owned, cooperatively polled tailnet.
use crate::{
    network::{self, Network},
    tcp::Socket,
    timestamp::EpochStore,
};
use std::{
    io::{self, Read, Write},
    net::SocketAddrV4,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Adapters must keep operations inside the coordinator's authorization boundary.
/// `pump` performs bounded, nonblocking transport/control work and returns promptly.
/// `run` preserves the I/O error kind carried by `network::Error::Io`.
pub trait Driver {
    type Store: EpochStore;
    fn run<R>(
        &mut self,
        operation: impl FnOnce(&mut Network<Self::Store>, u64) -> Result<R, network::Error>,
    ) -> io::Result<R>;
    fn pump(&mut self) -> io::Result<()>;
    fn abort(&mut self, socket: Socket) -> io::Result<()> {
        self.run(|network, _| match network.abort_tcp(&socket) {
            Err(network::Error::Tcp(crate::tcp::Error::State)) => Ok(()),
            result => result,
        })
    }
}
pub struct Stream<D: Driver> {
    driver: D,
    socket: Option<Socket>,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    failed: bool,
}
fn boundary(deadline: Instant, cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        return Err(io::ErrorKind::Interrupted.into());
    }
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(())
}
impl<D: Driver> Stream<D> {
    /// Deadline covers resolution, handshake, and every subsequent I/O operation.
    pub fn connect(
        driver: D,
        name: &str,
        port: u16,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        let name = crate::coordinator::dns_name(name)
            .map_err(|_| io::ErrorKind::InvalidInput)?
            .ok_or(io::ErrorKind::InvalidInput)?;
        if port == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut stream = Self {
            driver,
            socket: None,
            deadline,
            cancelled,
            failed: false,
        };
        loop {
            stream.check()?;
            if stream.socket.is_none() {
                let result = stream.driver.run(|network, now| {
                    let Some((peer, address)) = network.resolve(&name, now)? else {
                        return Ok(None);
                    };
                    Ok(Some(network.connect_tcp(
                        peer,
                        SocketAddrV4::new(address, port),
                        now,
                    )?))
                });
                match result {
                    Ok(socket) => stream.socket = socket,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                    Err(error) => return Err(error),
                }
            }
            if let Some(socket) = stream.socket.as_ref() {
                let result = stream.driver.run(|network, now| {
                    network.poll_tcp(socket, now)?;
                    network.tcp_state(socket, now)
                });
                let state = match result {
                    Ok(state) => state,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        stream.progress()?;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                match state {
                    smoltcp::socket::tcp::State::Established
                    | smoltcp::socket::tcp::State::CloseWait => {
                        stream.check()?;
                        return Ok(stream);
                    }
                    smoltcp::socket::tcp::State::Closed => {
                        return Err(io::ErrorKind::ConnectionRefused.into());
                    }
                    _ => (),
                }
            }
            stream.progress()?;
        }
    }
    /// Revalidates a stream before exposing plaintext retained by a TLS adapter.
    pub fn authorize(&mut self) -> io::Result<()> {
        loop {
            self.check()?;
            let socket = self.socket.as_ref().ok_or(io::ErrorKind::NotConnected)?;
            match self
                .driver
                .run(|network, now| network.tcp_state(socket, now).map(|_| ()))
            {
                Ok(()) => return self.check(),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => self.progress()?,
                Err(error) => {
                    self.abort();
                    return Err(error);
                }
            }
        }
    }
    fn check(&mut self) -> io::Result<()> {
        if self.failed {
            return Err(io::ErrorKind::NotConnected.into());
        }
        if let Err(error) = boundary(self.deadline, &self.cancelled) {
            self.abort();
            return Err(error);
        }
        Ok(())
    }
    fn abort(&mut self) {
        self.failed = true;
        if let Some(socket) = self.socket.take() {
            let _ = self.driver.abort(socket);
        }
    }
    fn progress(&mut self) -> io::Result<()> {
        self.check()?;
        if let Err(error) = self.driver.pump()
            && error.kind() != io::ErrorKind::WouldBlock
        {
            self.abort();
            return Err(error);
        }
        self.check()?;
        std::thread::sleep(
            Duration::from_millis(1).min(self.deadline.saturating_duration_since(Instant::now())),
        );
        Ok(())
    }
    fn transfer(
        &mut self,
        mut operation: impl FnMut(&mut Network<D::Store>, &Socket, u64) -> Result<usize, network::Error>,
    ) -> io::Result<usize> {
        loop {
            self.check()?;
            let socket = self.socket.as_ref().ok_or(io::ErrorKind::NotConnected)?;
            let result = self.driver.run(|network, now| {
                network.poll_tcp(socket, now)?;
                operation(network, socket, now)
            });
            match result {
                Ok(n) => {
                    self.check()?;
                    return Ok(n);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => self.progress()?,
                Err(error) => {
                    self.abort();
                    return Err(error);
                }
            }
        }
    }
}
impl<D: Driver> Read for Stream<D> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let mut staged = [0; 1024];
        let length = bytes.len().min(staged.len());
        let count = self.transfer(|network, socket, now| {
            network.read_tcp_io(socket, &mut staged[..length], now)
        })?;
        bytes[..count].copy_from_slice(&staged[..count]);
        Ok(count)
    }
}
impl<D: Driver> Write for Stream<D> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.transfer(|network, socket, now| network.write_tcp_io(socket, bytes, now))
    }
    /// Matches socket flush semantics: accepted data remains in the TCP send buffer.
    fn flush(&mut self) -> io::Result<()> {
        self.check()
    }
}
impl<D: Driver> Drop for Stream<D> {
    fn drop(&mut self) {
        self.abort();
    }
}

/// Single-thread runtime sharing. Each stream retains its own deadline and cancellation.
pub struct Shared<D: Driver> {
    driver: std::rc::Rc<std::cell::RefCell<D>>,
    pending_abort: std::rc::Rc<std::cell::RefCell<Vec<Socket>>>,
}
impl<D: Driver> Clone for Shared<D> {
    fn clone(&self) -> Self {
        Self {
            driver: self.driver.clone(),
            pending_abort: self.pending_abort.clone(),
        }
    }
}
impl<D: Driver> Shared<D> {
    pub fn new(driver: D) -> Self {
        Self {
            driver: std::rc::Rc::new(std::cell::RefCell::new(driver)),
            pending_abort: Default::default(),
        }
    }
    fn drain(&self, driver: &mut D) -> io::Result<()> {
        for _ in 0..4 {
            let Some(socket) = self.pending_abort.borrow_mut().pop() else {
                break;
            };
            if let Err(error) = driver.abort(socket.clone()) {
                if error.kind() == io::ErrorKind::WouldBlock {
                    self.pending_abort.borrow_mut().push(socket);
                }
                return Err(error);
            }
        }
        Ok(())
    }
}
impl<D: Driver> Driver for Shared<D> {
    type Store = D::Store;
    fn run<R>(
        &mut self,
        operation: impl FnOnce(&mut Network<Self::Store>, u64) -> Result<R, network::Error>,
    ) -> io::Result<R> {
        let mut driver = self
            .driver
            .try_borrow_mut()
            .map_err(|_| io::ErrorKind::WouldBlock)?;
        self.drain(&mut driver)?;
        let result = driver.run(operation);
        let cleanup = self.drain(&mut driver);
        match result {
            Ok(value) => cleanup.map(|_| value),
            Err(error) => Err(error),
        }
    }
    fn pump(&mut self) -> io::Result<()> {
        let mut driver = self
            .driver
            .try_borrow_mut()
            .map_err(|_| io::ErrorKind::WouldBlock)?;
        self.drain(&mut driver)?;
        let result = driver.pump();
        let cleanup = self.drain(&mut driver);
        result.and(cleanup)
    }
    fn abort(&mut self, socket: Socket) -> io::Result<()> {
        {
            let mut pending = self.pending_abort.borrow_mut();
            if !pending
                .iter()
                .any(|other| Arc::ptr_eq(&other.identity, &socket.identity))
            {
                if pending.len() == 4 {
                    return Err(io::ErrorKind::OutOfMemory.into());
                }
                pending.push(socket);
            }
        }
        // A stream can be dropped inside another operation. Cleanup follows that operation.
        if let Ok(mut driver) = self.driver.try_borrow_mut() {
            self.drain(&mut driver)?;
        }
        Ok(())
    }
}

/// Thread-safe runtime sharing with nonblocking lock acquisition. Callers retry
/// WouldBlock under their own deadline. Driver callbacks must return promptly.
pub struct ThreadShared<D: Driver> {
    driver: Arc<std::sync::Mutex<D>>,
    pending_abort: Arc<std::sync::Mutex<Vec<Socket>>>,
}
/// A request factory may retain a weak generation without retaining device storage.
pub struct ThreadWeak<D: Driver> {
    driver: std::sync::Weak<std::sync::Mutex<D>>,
    pending_abort: std::sync::Weak<std::sync::Mutex<Vec<Socket>>>,
}
impl<D: Driver> Clone for ThreadWeak<D> {
    fn clone(&self) -> Self {
        Self {
            driver: self.driver.clone(),
            pending_abort: self.pending_abort.clone(),
        }
    }
}
impl<D: Driver> ThreadWeak<D> {
    pub fn upgrade(&self) -> Option<ThreadShared<D>> {
        Some(ThreadShared {
            driver: self.driver.upgrade()?,
            pending_abort: self.pending_abort.upgrade()?,
        })
    }
}
impl<D: Driver> Clone for ThreadShared<D> {
    fn clone(&self) -> Self {
        Self {
            driver: self.driver.clone(),
            pending_abort: self.pending_abort.clone(),
        }
    }
}
impl<D: Driver> ThreadShared<D> {
    pub fn downgrade(&self) -> ThreadWeak<D> {
        ThreadWeak {
            driver: Arc::downgrade(&self.driver),
            pending_abort: Arc::downgrade(&self.pending_abort),
        }
    }
    pub fn new(driver: D) -> Self {
        Self {
            driver: Arc::new(std::sync::Mutex::new(driver)),
            pending_abort: Default::default(),
        }
    }
    /// Accesses runtime status or lifecycle without holding a lock across retry.
    /// Nested access returns WouldBlock. Dropping streams here defers their abort.
    pub fn with<R>(&self, operation: impl FnOnce(&mut D) -> io::Result<R>) -> io::Result<R> {
        let mut driver = self.driver.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
            std::sync::TryLockError::Poisoned(_) => {
                io::Error::from(io::ErrorKind::ConnectionAborted)
            }
        })?;
        operation(&mut driver)
    }
    /// Lifecycle cleanup may access a poisoned driver to close its resources.
    /// The poison flag remains set; ordinary operations continue to fail closed.
    /// Contention returns WouldBlock, including during cleanup.
    pub fn with_cleanup<R>(
        &self,
        operation: impl FnOnce(&mut D) -> io::Result<R>,
    ) -> io::Result<R> {
        let mut driver = match self.driver.try_lock() {
            Ok(driver) => driver,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err(io::ErrorKind::WouldBlock.into());
            }
        };
        operation(&mut driver)
    }
    fn defer(&self, socket: Socket) -> io::Result<()> {
        let mut pending = self
            .pending_abort
            .lock()
            .map_err(|_| io::ErrorKind::ConnectionAborted)?;
        if !pending
            .iter()
            .any(|other| Arc::ptr_eq(&other.identity, &socket.identity))
        {
            if pending.len() >= 4 {
                return Err(io::ErrorKind::OutOfMemory.into());
            }
            pending.push(socket);
        }
        Ok(())
    }
    fn drain(&self, driver: &mut D) -> io::Result<()> {
        for _ in 0..4 {
            let socket = self
                .pending_abort
                .lock()
                .map_err(|_| io::ErrorKind::ConnectionAborted)?
                .pop();
            let Some(socket) = socket else {
                break;
            };
            if let Err(error) = driver.abort(socket.clone()) {
                if error.kind() == io::ErrorKind::WouldBlock {
                    self.defer(socket)?;
                }
                return Err(error);
            }
        }
        Ok(())
    }
}
impl<D: Driver> Driver for ThreadShared<D> {
    type Store = D::Store;
    fn run<R>(
        &mut self,
        operation: impl FnOnce(&mut Network<Self::Store>, u64) -> Result<R, network::Error>,
    ) -> io::Result<R> {
        self.with(|driver| {
            self.drain(driver)?;
            let result = driver.run(operation);
            let cleanup = self.drain(driver);
            match result {
                Ok(value) => cleanup.map(|_| value),
                Err(error) => Err(error),
            }
        })
    }
    fn pump(&mut self) -> io::Result<()> {
        self.with(|driver| {
            self.drain(driver)?;
            let result = driver.pump();
            result.and(self.drain(driver))
        })
    }
    fn abort(&mut self, socket: Socket) -> io::Result<()> {
        self.defer(socket)?;
        match self.with(|driver| self.drain(driver)) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
            result => result,
        }
    }
}

#[cfg(test)]
mod thread_tests {
    use super::*;
    struct Store;
    impl EpochStore for Store {
        type Error = ();
        fn load_epoch(&mut self) -> Result<Option<u64>, ()> {
            Ok(None)
        }
        fn reserve_epoch(&mut self, _: u64) -> Result<(), ()> {
            Ok(())
        }
    }
    struct Unused;
    impl Driver for Unused {
        type Store = Store;
        fn run<R>(
            &mut self,
            _: impl FnOnce(&mut Network<Store>, u64) -> Result<R, network::Error>,
        ) -> io::Result<R> {
            panic!("contended driver must not be accessed")
        }
        fn pump(&mut self) -> io::Result<()> {
            panic!("contended driver must not be accessed")
        }
    }
    #[test]
    fn contended_connect_retries_until_deadline_and_poison_fails_closed() {
        let shared = ThreadShared::new(Unused);
        let other = shared.clone();
        shared
            .with(|_| {
                let worker = std::thread::spawn(move || {
                    Stream::connect(
                        other,
                        "peer.example",
                        80,
                        Instant::now() + Duration::from_millis(20),
                        Arc::new(AtomicBool::new(false)),
                    )
                    .err()
                    .unwrap()
                    .kind()
                });
                assert_eq!(worker.join().unwrap(), io::ErrorKind::TimedOut);
                Ok(())
            })
            .unwrap();
        let other = shared.clone();
        assert!(
            std::thread::spawn(move || other.with(|_| -> io::Result<()> { panic!("poison") }))
                .join()
                .is_err()
        );
        let mut cleaned = false;
        shared
            .with_cleanup(|_| {
                cleaned = true;
                Ok(())
            })
            .unwrap();
        assert!(cleaned);
        assert_eq!(
            shared.with(|_| Ok(())).unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
    }
}
