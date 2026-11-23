//! One in-flight catalog request, with explicit cancellation and joined teardown.
use crate::{
    Catalog,
    auth::BasicAuth,
    deadline::{Budget, Limit},
    http::{self, Transport},
};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub struct Completion<R = Catalog> {
    pub id: u64,
    pub result: Result<R, http::Error>,
}
struct Active<R> {
    id: u64,
    cancelled: Arc<AtomicBool>,
    budget: Budget,
    task: JoinHandle<Result<R, http::Error>>,
}
pub struct Worker<R = Catalog> {
    active: Option<Active<R>>,
    next_id: u64,
}
impl<R> Default for Worker<R> {
    fn default() -> Self {
        Self {
            active: None,
            next_id: 0,
        }
    }
}
impl Worker<Catalog> {
    /// The factory runs inside the worker; the transport itself need not be Send.
    pub fn fetch<T: Transport + 'static>(
        &mut self,
        source: &str,
        auth: Option<BasicAuth>,
        limit: Limit,
        factory: impl FnOnce() -> io::Result<T> + Send + 'static,
    ) -> io::Result<u64> {
        if self.is_busy() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "catalog worker busy",
            ));
        }
        let url = crate::web_url(None, source).map_err(io::Error::other)?;
        if url.fragment().is_some() || url.port_or_known_default() == Some(0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid catalog URL",
            ));
        }
        let source = url.to_string();
        self.submit(limit, move |budget, cancelled| {
            let mut transport = factory()?;
            budget.check()?;
            http::fetch_authenticated(&mut transport, &source, limit, cancelled, auth.as_ref())
        })
    }
}
impl<R: Send + 'static> Worker<R> {
    /// Results remain owned by the worker until delivery. Dropping a cancelled
    /// result must roll back any unpublished resources owned by that result.
    pub fn submit(
        &mut self,
        limit: Limit,
        operation: impl FnOnce(Budget, Arc<AtomicBool>) -> Result<R, http::Error> + Send + 'static,
    ) -> io::Result<u64> {
        if self.is_busy() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "catalog worker busy",
            ));
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let budget = Budget::new(limit, cancelled.clone());
        budget.check()?;
        let id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("catalog request IDs exhausted"))?;
        let task_budget = budget.clone();
        let task_cancel = cancelled.clone();
        let task = thread::Builder::new()
            .name("femto-catalog".into())
            // Host tailnet/TLS calls include large debug-build protocol frames.
            .stack_size(if cfg!(target_os = "espidf") {
                64 * 1024
            } else {
                2 * 1024 * 1024
            })
            .spawn(move || {
                task_budget.check()?;
                let result = operation(task_budget.clone(), task_cancel);
                task_budget.check()?;
                result
            })?;
        self.next_id = id;
        self.active = Some(Active {
            id,
            cancelled,
            budget,
            task,
        });
        Ok(id)
    }
}
impl<R> Worker<R> {
    pub fn is_busy(&self) -> bool {
        self.active.is_some()
    }
    /// Returns immediately while a request or an uncancellable platform call runs.
    pub fn poll(&mut self) -> Option<Completion<R>> {
        if !self.active.as_ref()?.task.is_finished() {
            return None;
        }
        let active = self.active.take()?;
        let result = active
            .task
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("catalog worker panicked").into()));
        // Cancellation after completion but before delivery still discards the result.
        let result = active.budget.check().map_err(http::Error::from).and(result);
        Some(Completion {
            id: active.id,
            result,
        })
    }

    pub fn cancel(&self) {
        if let Some(active) = &self.active {
            active.cancelled.store(true, Ordering::Release);
        }
    }

    /// A timeout retains ownership. The caller must abort sleep until this succeeds.
    pub fn cancel_and_wait(&mut self, timeout: Duration) -> bool {
        self.cancel();
        let start = Instant::now();
        while self.is_busy() {
            if self.poll().is_some() {
                return true;
            }
            if start.elapsed() >= timeout {
                return false;
            }
            thread::sleep(Duration::from_millis(1));
        }
        true
    }
}
impl<R> Drop for Worker<R> {
    fn drop(&mut self) {
        self.cancel();
        if let Some(active) = self.active.take() {
            // No detached task may retain sockets or storage after its owner drops.
            let _ = active.task.join();
        }
    }
}
