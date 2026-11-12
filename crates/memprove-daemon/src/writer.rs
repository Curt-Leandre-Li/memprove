//! Per-Project writer lanes.
//!
//! Protected operations run concurrently on Tokio's blocking pool. Every
//! mutation of a Project's `state.sqlite` runs inside that Project's lane, so
//! the daemon's own writers to one Project are serialized in-process and wait
//! on a mutex instead of on `SQLite`'s busy handler. The lane is a scheduling
//! and observability layer only: correctness does not depend on it, because
//! each store mutation is a single `BEGIN IMMEDIATE` transaction and the 0006
//! triggers and unique `project_sequence` index reject any fork of the Event
//! chain. Registrations are additionally serialized by the provisioner lock.
//!
//! `writer_queue_depth` reports the mutations currently inside a lane, waiting
//! or executing.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

use memprove_engine::runtime::WriterQueueDepth;

/// Which writer lane a mutation runs in.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Lane {
    /// Project registration (provisioning plus the root Workspace).
    Registration,
    /// One Project's state database, by Project id.
    Project(String),
}

/// Writer lanes and their admitted-mutation count.
#[derive(Debug, Default)]
pub struct WriterQueue {
    depth: AtomicU64,
    lanes: Mutex<HashMap<Lane, Arc<Mutex<()>>>>,
}

struct Admitted<'a>(&'a AtomicU64);

impl Drop for Admitted<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl WriterQueue {
    /// Runs `work` alone in `lane`, counting it in the depth from admission
    /// until it returns (or panics).
    pub fn run<T>(&self, lane: Lane, work: impl FnOnce() -> T) -> T {
        self.depth.fetch_add(1, Ordering::SeqCst);
        let _admitted = Admitted(&self.depth);
        let lane = Arc::clone(
            self.lanes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(lane)
                .or_default(),
        );
        let _exclusive = lane.lock().unwrap_or_else(PoisonError::into_inner);
        work()
    }
}

impl WriterQueueDepth for WriterQueue {
    fn depth(&self) -> u64 {
        self.depth.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread, time::Duration};

    use super::*;

    #[test]
    fn one_lane_runs_one_mutation_at_a_time_and_counts_waiters() {
        let queue = Arc::new(WriterQueue::default());
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let holder = {
            let queue = Arc::clone(&queue);
            thread::spawn(move || {
                queue.run(Lane::Project("p".to_owned()), || {
                    entered.send(()).unwrap();
                    release_rx.recv().unwrap();
                });
            })
        };
        entered_rx.recv().unwrap();
        let waiter = {
            let queue = Arc::clone(&queue);
            thread::spawn(move || queue.run(Lane::Project("p".to_owned()), || 7))
        };
        while queue.depth() < 2 {
            thread::sleep(Duration::from_millis(5));
        }
        // Another Project's lane is independent of the blocked one.
        assert_eq!(
            queue.run(Lane::Project("q".to_owned()), || queue.depth()),
            3
        );
        assert!(!waiter.is_finished());
        release.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(waiter.join().unwrap(), 7);
        assert_eq!(queue.depth(), 0);
    }

    #[test]
    fn a_panicking_mutation_leaves_the_queue_usable() {
        let queue = Arc::new(WriterQueue::default());
        let panicking = Arc::clone(&queue);
        assert!(
            thread::spawn(move || panicking.run(Lane::Registration, || panic!("boom")))
                .join()
                .is_err()
        );
        assert_eq!(queue.depth(), 0);
        assert_eq!(queue.run(Lane::Registration, || 1), 1);
    }
}
