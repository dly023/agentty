//! Resource accounting for the local headless runtime.
//!
//! The GUI is not a lease. Durable panes and in-flight mutations are. Keeping
//! this state independent from sockets makes the shutdown decision deterministic
//! and testable without launching a server.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct RuntimeLease {
    inner: Arc<Inner>,
}

struct Inner {
    grace: Duration,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    live_panes: u64,
    operations: u64,
    idle_since: Option<Instant>,
}

impl RuntimeLease {
    pub fn new(now: Instant, grace: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                grace,
                state: Mutex::new(State {
                    live_panes: 0,
                    operations: 0,
                    idle_since: Some(now),
                }),
            }),
        }
    }

    pub fn set_live_panes(&self, count: u64, now: Instant) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.live_panes = count;
        update_idle_since(&mut state, now);
    }

    pub fn begin_operation(&self, now: Instant) -> OperationLease {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.operations = state.operations.saturating_add(1);
        state.idle_since = None;
        OperationLease {
            lease: Some(self.clone()),
            finished_at: None,
            started_at: now,
        }
    }

    pub fn should_shutdown(&self, now: Instant) -> bool {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.live_panes == 0
            && state.operations == 0
            && state
                .idle_since
                .is_some_and(|idle| now.saturating_duration_since(idle) >= self.inner.grace)
    }

    pub fn snapshot(&self) -> RuntimeLeaseSnapshot {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        RuntimeLeaseSnapshot {
            live_panes: state.live_panes,
            operations: state.operations,
            idle: state.idle_since.is_some(),
        }
    }

    pub fn grace(&self) -> Duration {
        self.inner.grace
    }

    fn finish_operation(&self, now: Instant) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.operations = state.operations.saturating_sub(1);
        update_idle_since(&mut state, now);
    }
}

fn update_idle_since(state: &mut State, now: Instant) {
    if state.live_panes == 0 && state.operations == 0 {
        state.idle_since.get_or_insert(now);
    } else {
        state.idle_since = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeLeaseSnapshot {
    pub live_panes: u64,
    pub operations: u64,
    pub idle: bool,
}

pub struct OperationLease {
    lease: Option<RuntimeLease>,
    finished_at: Option<Instant>,
    #[allow(dead_code)]
    started_at: Instant,
}

impl OperationLease {
    pub fn finish_at(mut self, now: Instant) {
        self.finished_at = Some(now);
        if let Some(lease) = self.lease.take() {
            lease.finish_operation(now);
        }
    }
}

impl Drop for OperationLease {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            lease.finish_operation(self.finished_at.unwrap_or_else(Instant::now));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_resource_runtime_exits_after_grace() {
        let start = Instant::now();
        let lease = RuntimeLease::new(start, Duration::from_secs(5));
        assert!(!lease.should_shutdown(start + Duration::from_secs(4)));
        assert!(lease.should_shutdown(start + Duration::from_secs(5)));
    }

    #[test]
    fn live_pane_cancels_idle_shutdown() {
        let start = Instant::now();
        let lease = RuntimeLease::new(start, Duration::from_secs(5));
        lease.set_live_panes(1, start + Duration::from_secs(4));
        assert!(!lease.should_shutdown(start + Duration::from_secs(20)));
        lease.set_live_panes(0, start + Duration::from_secs(20));
        assert!(!lease.should_shutdown(start + Duration::from_secs(24)));
        assert!(lease.should_shutdown(start + Duration::from_secs(25)));
    }

    #[test]
    fn in_flight_operation_holds_the_runtime() {
        let start = Instant::now();
        let lease = RuntimeLease::new(start, Duration::from_secs(5));
        let operation = lease.begin_operation(start + Duration::from_secs(4));
        assert!(!lease.should_shutdown(start + Duration::from_secs(30)));
        operation.finish_at(start + Duration::from_secs(30));
        assert!(!lease.should_shutdown(start + Duration::from_secs(34)));
        assert!(lease.should_shutdown(start + Duration::from_secs(35)));
    }
}
