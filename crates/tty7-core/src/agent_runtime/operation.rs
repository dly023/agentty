use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use super::OperationId;

/// Transport-independent limits for a helper operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationLimits {
    pub max_items: usize,
    pub timeout_ms: u64,
}

impl OperationLimits {
    pub const fn bounded(max_items: usize, timeout_ms: u64) -> Self {
        Self {
            max_items,
            timeout_ms,
        }
    }
}

/// Tracks cancellation independently from a transport request id.
#[derive(Default, Debug)]
pub struct OperationRegistry {
    states: HashMap<OperationId, OperationState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OperationState {
    Running,
    Cancelled,
}

impl OperationRegistry {
    pub fn begin(&mut self, operation: OperationId) -> bool {
        if self.states.contains_key(&operation) {
            return false;
        }
        self.states.insert(operation, OperationState::Running);
        true
    }

    pub fn cancel(&mut self, operation: OperationId) -> bool {
        match self.states.get_mut(&operation) {
            Some(state) => {
                *state = OperationState::Cancelled;
                true
            }
            None => false,
        }
    }

    pub fn is_cancelled(&self, operation: OperationId) -> bool {
        self.states.get(&operation) == Some(&OperationState::Cancelled)
    }

    pub fn finish(&mut self, operation: OperationId) {
        self.states.remove(&operation);
    }
}

/// A bounded FIFO used by activity and candidate streams. New data is rejected
/// rather than silently discarding older authoritative events.
#[derive(Debug)]
pub struct BoundedBatch<T> {
    capacity: usize,
    items: VecDeque<T>,
}

impl<T> BoundedBatch<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            items: VecDeque::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, item: T) -> Result<(), T> {
        if self.items.len() >= self.capacity {
            return Err(item);
        }
        self.items.push_back(item);
        Ok(())
    }

    pub fn into_vec(self) -> Vec<T> {
        self.items.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_uses_operation_identity() {
        let mut registry = OperationRegistry::default();
        let operation = OperationId(7);
        assert!(registry.begin(operation));
        assert!(!registry.begin(operation));
        assert!(registry.cancel(operation));
        assert!(registry.is_cancelled(operation));
        registry.finish(operation);
        assert!(!registry.is_cancelled(operation));
    }

    #[test]
    fn bounded_batch_rejects_overflow_without_dropping_old_items() {
        let mut batch = BoundedBatch::new(2);
        assert_eq!(batch.push(1), Ok(()));
        assert_eq!(batch.push(2), Ok(()));
        assert_eq!(batch.push(3), Err(3));
        assert_eq!(batch.into_vec(), vec![1, 2]);
    }
}
