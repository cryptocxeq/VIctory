//! Synthetic tests through the guest list and queue function tables.

use super::*;
use crate::object::slot;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

struct Owned(This);

impl Drop for Owned {
    fn drop(&mut self) {
        slot::<extern "system" fn(This) -> u32>(self.0, 0x10)(self.0);
    }
}

struct Recording {
    list: Owned,
    allocator: Owned,
    queue: Owned,
    signal_slot: usize,
}

impl Recording {
    fn new(newer: bool) -> Self {
        crate::host::host().expect("memory-wait tests require a D3D12 device");
        let null = std::ptr::null();
        let mut allocator = null;
        assert_eq!(
            create_allocator(null, 0, &ALLOCATOR_IIDS[0], &mut allocator),
            S_OK
        );
        let mut list = null;
        assert_eq!(
            create_list(null, 0, 0, allocator, null, &LIST_IIDS[1], &mut list),
            S_OK
        );
        // Select each ABI explicitly, without changing the process-wide layout.
        // SAFETY: a new, exclusively owned list; both tables have static storage.
        unsafe {
            let table = if newer {
                NEWER_LIST_FUNCTIONS.0.as_ptr()
            } else {
                LIST_FUNCTIONS.0.as_ptr()
            };
            list.cast_mut().cast::<*const *const ()>().write(table);
        }
        let (queue, signal_slot) = crate::queue::test_queue();
        Self {
            list: Owned(list),
            allocator: Owned(allocator),
            queue: Owned(queue),
            signal_slot,
        }
    }

    fn list_slot(&self, older: usize) -> usize {
        // SAFETY: a live list whose table was selected above.
        let table = unsafe { *self.list.0.cast::<*const *const ()>() };
        if table == NEWER_LIST_FUNCTIONS.0.as_ptr() {
            newer_slot(older) * 8
        } else {
            older * 8
        }
    }

    fn write(&self, at: &AtomicU64, value: u64) {
        slot::<extern "system" fn(This, u64, u64, u32, u32)>(self.list.0, self.list_slot(70))(
            self.list.0,
            at.as_ptr() as u64,
            value,
            0,
            0,
        );
    }

    fn wait(&self, at: &AtomicU64, comparison: u32, reference: u64, flags: u32) {
        slot::<extern "system" fn(This, u64, u32, u64, u32)>(self.list.0, self.list_slot(72))(
            self.list.0,
            at.as_ptr() as u64,
            comparison,
            reference,
            flags,
        );
    }

    fn copy(&self, dst: &AtomicU64, src: &AtomicU64) {
        slot::<extern "system" fn(This, u64, u64, u64, u32)>(self.list.0, self.list_slot(60))(
            self.list.0,
            dst.as_ptr() as u64,
            src.as_ptr() as u64,
            8,
            0,
        );
    }

    fn submit(&self) {
        assert_eq!(close(self.list.0), S_OK);
        slot::<extern "system" fn(This, u32, *const This)>(self.queue.0, 0x50)(
            self.queue.0,
            1,
            &self.list.0,
        );
    }

    fn signal(&self, fence: &Owned) {
        assert_eq!(
            slot::<extern "system" fn(This, This, u64) -> HRESULT>(self.queue.0, self.signal_slot,)(
                self.queue.0,
                fence.0,
                1
            ),
            S_OK
        );
    }
}

fn done_fence() -> Owned {
    let mut fence = std::ptr::null();
    assert_eq!(
        crate::fence::create(
            std::ptr::null(),
            0,
            0,
            &crate::fence::FENCE_IIDS[0],
            &mut fence
        ),
        S_OK
    );
    Owned(fence)
}

fn completed(fence: &Owned) -> u64 {
    slot::<extern "system" fn(This) -> u64>(fence.0, 0x40)(fence.0)
}

fn finish(fence: &Owned) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while completed(fence) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        completed(fence),
        1,
        "queue did not resume after its memory wait"
    );
}

#[test]
fn memory_wait_orders_two_queues() {
    if crate::host::host().is_none() {
        return;
    }
    for newer in [false, true] {
        for consumer_first in [false, true] {
            for (comparison, counter_value, reference, flags) in
                [(7, 2, 1, 1), (3, 0x1_0000_0002, 0x1_0000_0002, 0)]
            {
                let (payload, counter, result) =
                    (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
                let (producer, consumer) = (Recording::new(newer), Recording::new(newer));
                let done = done_fence();
                producer.write(&payload, 0x5a55a11c);
                producer.write(&counter, counter_value);
                consumer.wait(&counter, comparison, reference, flags);
                consumer.copy(&result, &payload);
                if consumer_first {
                    consumer.submit();
                    consumer.signal(&done);
                    assert_eq!(completed(&done), 0, "wait must hold later queue signals");
                    producer.submit();
                } else {
                    producer.submit();
                    consumer.submit();
                    consumer.signal(&done);
                }
                finish(&done);
                assert_eq!(
                    result.load(Ordering::Acquire),
                    0x5a55a11c,
                    "newer={newer}, consumer_first={consumer_first}, comparison={comparison}"
                );
            }
        }
    }
}

#[test]
fn memory_wait_preserves_work_before_and_between_waits() {
    if crate::host::host().is_none() {
        return;
    }
    for newer in [false, true] {
        let (a, b, c, result) = (
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
        );
        let (first, second) = (Recording::new(newer), Recording::new(newer));
        let (first_done, second_done) = (done_fence(), done_fence());
        first.write(&a, 1);
        first.wait(&b, 3, 1, 0);
        first.copy(&result, &b);
        first.write(&c, 1);
        first.wait(&b, 7, 2, 1);
        first.copy(&result, &b);
        second.wait(&a, 3, 1, 0);
        second.write(&b, 1);
        second.wait(&c, 3, 1, 0);
        second.write(&b, 2);
        first.submit();
        first.signal(&first_done);
        assert_eq!(
            a.load(Ordering::Acquire),
            1,
            "the prefix must execute before the wait"
        );
        assert_eq!(completed(&first_done), 0);
        second.submit();
        second.signal(&second_done);
        finish(&first_done);
        finish(&second_done);
        assert_eq!(result.load(Ordering::Acquire), 2);
    }
}

#[test]
fn memory_wait_observes_cpu_writes_and_keeps_pending_recordings() {
    if crate::host::host().is_none() {
        return;
    }
    for drop_list in [false, true] {
        let (ready, result) = (AtomicU64::new(0), AtomicU64::new(0));
        let recording = Recording::new(true);
        let done = done_fence();
        // Allocate before the split, but read after it. Reset and release
        // must keep this upload block and every segment until completion.
        let upload = allocate_upload(recording.list.0, 8, 8);
        assert!(!upload.is_null());
        // SAFETY: eight writable bytes handed out above.
        unsafe { upload.cast::<u64>().write(0x1122334455667788) };
        recording.wait(&ready, 7, 1, 0);
        copy_memory(
            recording.list.0,
            result.as_ptr() as u64,
            upload as u64,
            8,
            0,
        );
        recording.submit();
        recording.signal(&done);
        assert_eq!(completed(&done), 0);
        if !drop_list {
            assert_eq!(
                reset(recording.list.0, recording.allocator.0, std::ptr::null()),
                S_OK
            );
            recording.write(&result, 99);
        }
        let Recording {
            list,
            allocator,
            queue,
            ..
        } = recording;
        drop(list);
        ready.store(1, Ordering::Release);
        finish(&done);
        assert_eq!(result.load(Ordering::Acquire), 0x1122334455667788);
        drop((allocator, queue));
    }
}

#[test]
fn memory_wait_compares_the_high_dword() {
    if crate::host::host().is_none() {
        return;
    }
    let (ready, result) = (AtomicU64::new(2), AtomicU64::new(0));
    let consumer = Recording::new(true);
    let done = done_fence();
    consumer.wait(&ready, 3, 0x1_0000_0002, 1);
    consumer.write(&result, 1);
    consumer.submit();
    consumer.signal(&done);
    assert_eq!(
        completed(&done),
        0,
        "matching low dwords do not satisfy a 64-bit wait"
    );
    ready.store(0x1_0000_0002, Ordering::Release);
    finish(&done);
    assert_eq!(result.load(Ordering::Acquire), 1);
}

#[test]
fn memory_wait_keeps_later_lists_and_submissions_behind_it() {
    if crate::host::host().is_none() {
        return;
    }
    let (ready, result) = (AtomicU64::new(0), AtomicU64::new(0));
    let first = Recording::new(true);
    let second = Recording::new(true);
    let third = Recording::new(true);
    let done = done_fence();
    first.wait(&ready, 7, 1, 0);
    first.write(&result, 1);
    second.write(&result, 2);
    third.write(&result, 3);
    for recording in [&first, &second, &third] {
        assert_eq!(close(recording.list.0), S_OK);
    }
    let execute = slot::<extern "system" fn(This, u32, *const This)>(first.queue.0, 0x50);
    execute(first.queue.0, 2, [first.list.0, second.list.0].as_ptr());
    execute(first.queue.0, 1, &third.list.0);
    first.signal(&done);
    assert_eq!(completed(&done), 0);
    ready.store(1, Ordering::Release);
    finish(&done);
    assert_eq!(result.load(Ordering::Acquire), 3);
}
