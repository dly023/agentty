//! Connection-owned reservations. Signal under the reader; retire off it.
use crate::agent_sessions::live_titles::{HistoryTitleEvent, HistoryTitleSubscription};
use std::collections::HashMap;
use std::io;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use uuid::Uuid;

#[derive(Default)]
pub(super) struct TitleWatches(Mutex<State>);

#[derive(Default)]
struct State {
    closed: bool,
    entries: HashMap<Uuid, Entry>,
}

struct Entry {
    request: u64,
    alive: Arc<AtomicBool>,
    source: Option<HistoryTitleSubscription>,
}

impl Entry {
    fn signal(&self) {
        self.alive.store(false, Ordering::Release);
        if let Some(source) = &self.source {
            source.events().close();
        }
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.signal();
    }
}

impl TitleWatches {
    pub(super) fn reserve(&self, request: u64, id: Uuid) -> io::Result<()> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "title connection closed",
            ));
        }
        if state.entries.contains_key(&id) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "title subscription already exists",
            ));
        }
        if state.entries.len() >= 16 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "too many title subscriptions",
            ));
        }
        state.entries.insert(
            id,
            Entry {
                request,
                alive: Arc::new(AtomicBool::new(true)),
                source: None,
            },
        );
        Ok(())
    }

    pub(super) fn install(
        &self,
        request: u64,
        id: Uuid,
        source: HistoryTitleSubscription,
    ) -> io::Result<(smol::channel::Receiver<HistoryTitleEvent>, Arc<AtomicBool>)> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let entry = state
            .entries
            .get_mut(&id)
            .filter(|e| {
                e.request == request && e.alive.load(Ordering::Acquire) && e.source.is_none()
            })
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::Interrupted, "title subscription retired")
            })?;
        let rx = source.events().clone();
        let alive = entry.alive.clone();
        entry.source = Some(source);
        Ok((rx, alive))
    }

    pub(super) fn signal_id(&self, id: Uuid) -> Option<u64> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .get(&id)
            .map(|entry| {
                entry.signal();
                entry.request
            })
    }

    pub(super) fn signal_request(&self, request: u64) -> bool {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut found = false;
        for entry in state.entries.values().filter(|e| e.request == request) {
            entry.signal();
            found = true;
        }
        found
    }

    pub(super) fn retire_request(&self, request: u64) {
        let retired = {
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            let id = state
                .entries
                .iter()
                .find_map(|(id, e)| (e.request == request).then_some(*id));
            id.and_then(|id| state.entries.remove(&id))
        };
        drop(retired);
    }

    pub(super) fn shutdown(&self) {
        let retired = {
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            state.closed = true;
            for entry in state.entries.values() {
                entry.signal();
            }
            std::mem::take(&mut state.entries)
        };
        drop(retired);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn source(drops: Arc<AtomicUsize>) -> HistoryTitleSubscription {
        struct Count(Arc<AtomicUsize>, smol::channel::Sender<HistoryTitleEvent>);
        impl Drop for Count {
            fn drop(&mut self) {
                assert!(self.1.is_closed());
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let (tx, rx) = smol::channel::bounded(1);
        HistoryTitleSubscription::new(rx, Box::new(Count(drops, tx)))
    }

    #[test]
    fn history_title_reservations_reject_late_install_and_foreign_cleanup() {
        let watches = TitleWatches::default();
        let id = uuid::Uuid::new_v4();
        let drops = Arc::new(AtomicUsize::new(0));
        watches.reserve(1, id).unwrap();
        assert!(watches.reserve(2, id).is_err());
        watches.retire_request(2);
        let (rx, alive) = watches.install(1, id, source(drops.clone())).unwrap();
        assert!(alive.load(Ordering::Acquire));
        assert!(!rx.is_closed());
        watches.signal_request(2);
        assert!(alive.load(Ordering::Acquire));
        watches.signal_id(id);
        assert!(!alive.load(Ordering::Acquire));
        assert!(rx.is_closed());
        watches.retire_request(1);
        assert_eq!(drops.load(Ordering::SeqCst), 1);

        watches.reserve(3, id).unwrap();
        watches.signal_id(id);
        assert!(watches.install(3, id, source(drops.clone())).is_err());
        watches.retire_request(3);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        watches.reserve(4, id).unwrap();
        watches.signal_request(4);
        watches.retire_request(4);
        assert!(watches.install(4, id, source(drops.clone())).is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn history_title_reservations_are_bounded_and_disconnect_closed() {
        let watches = TitleWatches::default();
        let ids: Vec<_> = (0..16).map(|_| uuid::Uuid::new_v4()).collect();
        for (n, id) in ids.iter().enumerate() {
            watches.reserve(n as u64, *id).unwrap();
        }
        assert!(watches.reserve(17, uuid::Uuid::new_v4()).is_err());
        let drops = Arc::new(AtomicUsize::new(0));
        let (rx, alive) = watches.install(0, ids[0], source(drops.clone())).unwrap();
        watches.shutdown();
        assert!(rx.is_closed());
        assert!(!alive.load(Ordering::Acquire));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(watches.reserve(18, uuid::Uuid::new_v4()).is_err());
        assert!(watches.install(1, ids[1], source(drops.clone())).is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }
}
