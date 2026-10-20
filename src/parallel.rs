//! A fixed pool of scoped threads over a slice, with one owner for progress.
//!
//! Workers share nothing mutable. Each takes the next index from an atomic counter,
//! keeps its own results, and *publishes* facts (bytes, file done, retry) on a channel.
//! The calling thread is the only writer of the `Tally` and the only caller of the
//! progress callback, so the callback needs neither `Send` nor a lock.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use crate::model::RelPath;
use crate::{Event, Tally};

const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// Index of a worker thread. Names the worker's own partial-download file.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WorkerId(pub usize);

pub(crate) enum Msg {
    Bytes(i64),
    FileDone { downloaded: bool, length: u64 },
    Retry { path: RelPath, attempt: u32, cause: String },
}

/// A worker's handle for publishing progress.
pub(crate) struct Sink {
    tx: mpsc::Sender<Msg>,
    worker: WorkerId,
}

impl Sink {
    pub(crate) fn worker(&self) -> WorkerId {
        self.worker
    }
    pub(crate) fn bytes(&self, delta: i64) {
        let _ = self.tx.send(Msg::Bytes(delta));
    }
    pub(crate) fn file_done(&self, downloaded: bool, length: u64) {
        let _ = self.tx.send(Msg::FileDone { downloaded, length });
    }
    pub(crate) fn retry(&self, path: RelPath, attempt: u32, cause: String) {
        let _ = self.tx.send(Msg::Retry { path, attempt, cause });
    }
    /// For the two control-file downloads on the calling thread. Their byte progress is
    /// not worth a channel; the caller reports a `Phase` around them instead.
    pub(crate) fn detached() -> Sink {
        let (tx, _rx) = mpsc::channel();
        Sink { tx, worker: WorkerId(0) }
    }
}

/// Run `work` over every item on `jobs` threads. Returns results in item order.
/// Emits `Event::Progress` at most every 100 ms, plus once at the end.
///
/// Ctrl-C is not handled: it kills the process, and a killed run is a crashed run,
/// which the next run converges from. No cancel flag exists to get wrong.
pub(crate) fn for_each<T: Sync, R: Send>(
    items: &[T],
    jobs: NonZeroUsize,
    mut tally: Tally,
    on_event: &mut dyn FnMut(Event),
    work: impl Fn(&T, &Sink) -> R + Sync,
) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel();
    let work = &work;
    let next = &next;
    let mut results: Vec<(usize, R)> = thread::scope(|scope| {
        let handles: Vec<_> = (0..jobs.get().min(items.len()))
            .map(|w| {
                let sink = Sink { tx: tx.clone(), worker: WorkerId(w) };
                scope.spawn(move || {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(i) else { break };
                        mine.push((i, work(item, &sink)));
                    }
                    mine
                })
            })
            .collect();
        drop(tx);
        let mut last = Instant::now();
        on_event(Event::Progress(tally));
        loop {
            match rx.recv_timeout(PROGRESS_EVERY) {
                Ok(Msg::Bytes(delta)) => {
                    tally.bytes_done = tally.bytes_done.saturating_add_signed(delta);
                    tally.bytes_downloaded = tally.bytes_downloaded.saturating_add_signed(delta);
                }
                Ok(Msg::FileDone { downloaded, length }) => {
                    tally.files_done += 1;
                    if !downloaded {
                        tally.bytes_done += length;
                    }
                }
                Ok(Msg::Retry { path, attempt, cause }) => on_event(Event::Retry { path, attempt, cause }),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if last.elapsed() >= PROGRESS_EVERY {
                on_event(Event::Progress(tally));
                last = Instant::now();
            }
        }
        on_event(Event::Progress(tally));
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)))
            .collect()
    });
    results.sort_unstable_by_key(|(i, _)| *i);
    results.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_keep_item_order_and_the_tally_sums_every_message() {
        let items: Vec<u64> = (0..100).collect();
        let mut last = Tally::default();
        let results = for_each(
            &items,
            NonZeroUsize::new(7).unwrap(),
            Tally { files_total: 100, ..Tally::default() },
            &mut |event| {
                if let Event::Progress(t) = event {
                    last = t;
                }
            },
            |&n, sink| {
                if n % 2 == 0 {
                    sink.bytes(n as i64 + 5);
                    sink.bytes(-5);
                    sink.file_done(true, n);
                } else {
                    sink.file_done(false, n);
                }
                n * 2
            },
        );
        assert_eq!(results, (0..100).map(|n| n * 2).collect::<Vec<_>>());
        assert_eq!(last.files_done, 100);
        assert_eq!(last.bytes_done, (0..100).sum::<u64>());
        assert_eq!(last.bytes_downloaded, (0..100).filter(|n| n % 2 == 0).sum::<u64>());
    }

    #[test]
    fn empty_input_spawns_nothing() {
        let results: Vec<()> =
            for_each(&[] as &[u8], NonZeroUsize::MIN, Tally::default(), &mut |_| {}, |_, _| ());
        assert!(results.is_empty());
    }
}
