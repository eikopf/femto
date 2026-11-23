//! Waiting limits and cancellation for nonblocking platform streams.
use std::{
    io::{self, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// How long a request may wait.
#[derive(Clone, Copy, Debug)]
pub enum Limit {
    /// The whole request ends at this instant: a caller that was itself
    /// given a time limit, such as a command-line tool.
    Until(Instant),
    /// Each wait for the other end may last this long. A request that keeps
    /// moving bytes never ends for its duration alone.
    Stall(Duration),
}
impl Limit {
    /// The reader's limit on a silent connection. A tailnet path that dies
    /// takes this long to replace: WireGuard waits 15 s for a reply before it
    /// starts a new handshake, an attempt takes up to 5 s, and a relay
    /// connection took up to 8 s to make on the reader. A connection silent
    /// for longer is not coming back.
    pub const SILENCE: Self = Self::Stall(Duration::from_secs(30));
    /// When a wait that starts now gives up.
    pub fn wait(self) -> Instant {
        match self {
            Self::Until(deadline) => deadline,
            Self::Stall(limit) => Instant::now() + limit,
        }
    }
    /// Whether the request as a whole is over. A stall is judged by the
    /// stream that waits, never between steps.
    pub fn expired(self) -> bool {
        matches!(self, Self::Until(deadline) if Instant::now() >= deadline)
    }
}
#[cfg(feature = "tailnet")]
impl From<Limit> for femto_microlink::application_tcp::Limit {
    fn from(limit: Limit) -> Self {
        match limit {
            Limit::Until(deadline) => Self::Until(deadline),
            Limit::Stall(limit) => Self::Stall(limit),
        }
    }
}

#[derive(Clone)]
pub struct Budget {
    limit: Limit,
    cancelled: Arc<AtomicBool>,
}
impl Budget {
    pub fn new(limit: Limit, cancelled: Arc<AtomicBool>) -> Self {
        Self { limit, cancelled }
    }
    pub fn limit(&self) -> Limit {
        self.limit
    }
    pub fn check(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "request cancelled",
            ));
        }
        if self.limit.expired() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "request deadline"));
        }
        Ok(())
    }
    /// The operation must return promptly. WouldBlock and Interrupted are
    /// retried until the limit on one wait runs out.
    pub fn retry<T>(&self, mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
        let until = self.limit.wait();
        loop {
            self.check()?;
            let result = operation();
            self.check()?;
            match result {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    let left = until.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "no answer from the server",
                        ));
                    }
                    std::thread::sleep(left.min(Duration::from_millis(5)));
                }
                result => return result,
            }
        }
    }
}

/// A terminal error drops the platform stream. Reads stage bytes until the final
/// deadline/cancellation check, so a rejected read does not change the caller's buffer.
pub struct Bounded<S> {
    stream: Option<S>,
    budget: Budget,
}
impl<S> Bounded<S> {
    pub fn new(stream: S, budget: Budget) -> Self {
        Self {
            stream: Some(stream),
            budget,
        }
    }
    /// Consumes the adapter after checking its current operation budget.
    /// The caller takes responsibility for future deadlines and cancellation.
    /// Buffered protocol state remains in the returned platform stream.
    pub fn into_nonblocking(mut self) -> io::Result<(S, Arc<AtomicBool>)> {
        self.budget.check()?;
        let stream = self
            .stream
            .take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "request stream closed"))?;
        Ok((stream, self.budget.cancelled.clone()))
    }
    /// Sets the next operation deadline without reopening a closed stream.
    pub fn set_deadline(&mut self, deadline: Instant) {
        self.budget.limit = Limit::Until(deadline);
    }
    fn operation<T>(
        &mut self,
        mut operation: impl FnMut(&mut S) -> io::Result<T>,
    ) -> io::Result<T> {
        let result = match self.stream.as_mut() {
            Some(stream) => self.budget.retry(|| operation(stream)),
            None => Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "request stream closed",
            )),
        };
        if result.is_err() {
            self.stream.take();
        }
        result
    }
}
impl<S: Read> Read for Bounded<S> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut staged = [0; 1024];
        let length = buffer.len().min(staged.len());
        let count = self.operation(|stream| stream.read(&mut staged[..length]))?;
        buffer[..count].copy_from_slice(&staged[..count]);
        Ok(count)
    }
}
impl<S: Write> Write for Bounded<S> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.operation(|stream| stream.write(buffer))
    }
    fn flush(&mut self) -> io::Result<()> {
        self.operation(Write::flush)
    }
}

#[cfg(test)]
mod renewal_tests {
    use super::*;
    #[test]
    fn new_operation_deadline_does_not_revive_closed_or_cancelled_stream() {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut stream = Bounded::new(
            io::Cursor::new(b"hello".to_vec()),
            Budget::new(Limit::Until(Instant::now()), cancel.clone()),
        );
        let mut byte = [0];
        assert_eq!(
            stream.read(&mut byte).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        stream.set_deadline(Instant::now() + Duration::from_secs(1));
        assert_eq!(
            stream.read(&mut byte).unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
        let mut stream = Bounded::new(
            io::Cursor::new(b"hello".to_vec()),
            Budget::new(Limit::Until(Instant::now()), cancel.clone()),
        );
        stream.set_deadline(Instant::now() + Duration::from_secs(1));
        assert_eq!(stream.read(&mut byte).unwrap(), 1);
        cancel.store(true, Ordering::Release);
        stream.set_deadline(Instant::now() + Duration::from_secs(1));
        assert_eq!(
            stream.read(&mut byte).unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
    }
}

#[cfg(test)]
mod handoff_tests {
    use super::*;
    #[test]
    fn nonblocking_handoff_preserves_stream_position_and_cancellation_owner() {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut bounded = Bounded::new(
            io::Cursor::new(b"hello".to_vec()),
            Budget::new(
                Limit::Until(Instant::now() + Duration::from_secs(1)),
                cancel.clone(),
            ),
        );
        let mut prefix = [0; 2];
        bounded.read_exact(&mut prefix).unwrap();
        let (mut stream, token) = bounded.into_nonblocking().unwrap();
        assert!(Arc::ptr_eq(&token, &cancel));
        let mut rest = String::new();
        stream.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, "llo");
    }
    #[test]
    fn failed_expired_and_cancelled_streams_cannot_be_handed_off() {
        let cancel = Arc::new(AtomicBool::new(false));
        let expired = || Budget::new(Limit::Until(Instant::now()), cancel.clone());
        assert!(
            Bounded::new(io::empty(), expired())
                .into_nonblocking()
                .is_err()
        );
        let mut failed = Bounded::new(io::empty(), expired());
        assert!(failed.read(&mut [0]).is_err());
        failed.set_deadline(Instant::now() + Duration::from_secs(1));
        assert_eq!(
            failed.into_nonblocking().unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
        cancel.store(true, Ordering::Release);
        assert_eq!(
            Bounded::new(
                io::empty(),
                Budget::new(
                    Limit::Until(Instant::now() + Duration::from_secs(1)),
                    cancel
                )
            )
            .into_nonblocking()
            .unwrap_err()
            .kind(),
            io::ErrorKind::ConnectionAborted
        );
    }
}

#[cfg(test)]
mod stall_tests {
    use super::*;
    /// Yields a byte, then blocks `silent` times before the next one.
    struct Halting {
        silent: usize,
        left: usize,
    }
    impl Read for Halting {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.left > 0 {
                self.left -= 1;
                return Err(io::ErrorKind::WouldBlock.into());
            }
            self.left = self.silent;
            buffer[0] = 1;
            Ok(1)
        }
    }
    #[test]
    fn stall_limit_ends_a_silent_stream_but_not_a_slow_one() {
        let cancel = Arc::new(AtomicBool::new(false));
        let limit = Limit::Stall(Duration::from_millis(40));
        // Each byte arrives after about 10 ms: the transfer outlasts the
        // limit several times over and still completes.
        let mut slow = Bounded::new(
            Halting { silent: 2, left: 0 },
            Budget::new(limit, cancel.clone()),
        );
        let start = Instant::now();
        let mut bytes = [0; 16];
        slow.read_exact(&mut bytes).unwrap();
        assert!(start.elapsed() > Duration::from_millis(80));
        let mut silent = Bounded::new(
            Halting {
                silent: usize::MAX,
                left: usize::MAX,
            },
            Budget::new(limit, cancel),
        );
        let start = Instant::now();
        assert_eq!(
            silent.read(&mut bytes).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
