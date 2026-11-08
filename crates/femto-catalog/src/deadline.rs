//! Deadline and cancellation adapter for nonblocking platform streams.
use std::{
    io::{self, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct Budget {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}
impl Budget {
    pub fn new(deadline: Instant, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            deadline,
            cancelled,
        }
    }
    pub fn check(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "request cancelled",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "request deadline"));
        }
        Ok(())
    }
    /// The operation must return promptly. WouldBlock and Interrupted are retried.
    pub fn retry<T>(&self, mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
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
                    std::thread::sleep(
                        self.deadline
                            .saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(5)),
                    );
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
        self.budget.deadline = deadline;
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
            Budget::new(Instant::now(), cancel.clone()),
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
            Budget::new(Instant::now(), cancel.clone()),
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
            Budget::new(Instant::now() + Duration::from_secs(1), cancel.clone()),
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
        let expired = || Budget::new(Instant::now(), cancel.clone());
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
                Budget::new(Instant::now() + Duration::from_secs(1), cancel)
            )
            .into_nonblocking()
            .unwrap_err()
            .kind(),
            io::ErrorKind::ConnectionAborted
        );
    }
}
