//! Command-list memory waits, polled only while a queue is held back.

use std::sync::{Arc, Condvar, Mutex, Once, Weak};
use std::time::Duration;

use crate::timeline::Timeline;

#[derive(Clone, Copy)]
pub(crate) struct Wait {
    pub(crate) address: u64,
    pub(crate) comparison: u32,
    pub(crate) reference: u64,
}

impl Wait {
    pub(crate) fn reached(self) -> bool {
        // These are D3D12_COMPARISON_FUNC values. ALWAYS/NEVER need no read.
        if self.comparison == 8 {
            return true;
        }
        if self.comparison == 1 {
            return false;
        }
        let mut bytes = [0; 8];
        // A kernel read also handles a title unmapping the word while we
        // poll it, without dereferencing its pointer in this process.
        if !crate::host::read_into(self.address as *const u8, &mut bytes) {
            return false;
        }
        let value = u64::from_ne_bytes(bytes);
        match self.comparison {
            2 => value < self.reference,
            3 => value == self.reference,
            4 => value <= self.reference,
            5 => value > self.reference,
            6 => value != self.reference,
            7 => value >= self.reference,
            _ => false,
        }
    }

    pub(crate) fn wait_on_gpu(self, timeline: &Arc<Timeline>) -> bool {
        if self.reached() {
            return true;
        }
        static START: Once = Once::new();
        START.call_once(|| {
            std::thread::Builder::new()
                .name("victory-memory-wait".into())
                .spawn(watch)
                .expect("could not start the GPU memory-wait watcher");
        });
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let weak = Arc::downgrade(timeline);
        if !pending.iter().any(|p| p.ptr_eq(&weak)) {
            pending.push(weak);
        }
        ADDED.notify_one();
        false
    }
}

static PENDING: Mutex<Vec<Weak<Timeline>>> = Mutex::new(Vec::new());
static ADDED: Condvar = Condvar::new();

fn watch() {
    loop {
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        while pending.is_empty() {
            pending = ADDED.wait(pending).unwrap_or_else(|e| e.into_inner());
        }
        let timelines = std::mem::take(&mut *pending);
        drop(pending);
        for timeline in timelines.into_iter().filter_map(|p| p.upgrade()) {
            // No watcher lock while entering a timeline. An unreached wait
            // registers itself again; a completed one is no longer polled.
            timeline.wake();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_wait_uses_d3d12_comparisons() {
        // NEVER, LESS, EQUAL, LESS_EQUAL, GREATER, NOT_EQUAL, GREATER_EQUAL, ALWAYS.
        for (value, expected) in [
            (
                0x1_0000_0001,
                [false, true, false, true, false, true, false, true],
            ),
            (
                0x1_0000_0002,
                [false, false, true, true, false, false, true, true],
            ),
            (
                u64::MAX,
                [false, false, false, false, true, true, true, true],
            ),
        ] {
            for (i, expected) in expected.into_iter().enumerate() {
                let wait = Wait {
                    address: std::ptr::from_ref(&value) as u64,
                    comparison: i as u32 + 1,
                    reference: 0x1_0000_0002,
                };
                assert_eq!(
                    wait.reached(),
                    expected,
                    "comparison={}, value={value:#x}",
                    i + 1
                );
            }
        }
        assert!(
            !Wait {
                address: 0,
                comparison: 7,
                reference: 1
            }
            .reached()
        );
    }
}
