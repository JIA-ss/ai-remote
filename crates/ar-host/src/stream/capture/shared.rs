//! One running capture per monitor, shared by everyone viewing that monitor: the first viewer starts
//! it, later viewers join it, the last one to leave stops it. Desktop Duplication needs this: a
//! process may duplicate an output only once, and a second `DuplicateOutput` on the same output
//! fails with E_INVALIDARG. Before this, a host running as SYSTEM (service mode, where Desktop
//! Duplication is the only capture path) showed a monitor to one viewer at a time; the second one
//! got an error (seen on 2026-10-22 on a Windows 11 test machine, see AGENTS.md).
//!
//! The last viewer out stops the producer and waits for its thread while holding the registry's
//! lock, so a viewer that reconnects at once (a quality change, a retry) starts only after the old
//! producer let go of what it held, instead of colliding with it.

use std::{
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
};

use anyhow::Result;

/// The running producers, one per key (a monitor). A `static`: viewers of one monitor arrive on
/// separate connections.
pub(super) struct Sources<K, V> {
    running: Mutex<Vec<(K, Arc<Source<V>>)>>,
    next_id: AtomicU64,
}

/// One producer and the viewers it hands its frames to.
pub(super) struct Source<V> {
    viewers: Mutex<Vec<(u64, V)>>,
    stop: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

/// One viewer's place at a producer; leaving when dropped (the last one stops the producer).
pub(super) struct Member<K: 'static, V: 'static> {
    sources: &'static Sources<K, V>,
    source: Arc<Source<V>>,
    id: u64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl<K: PartialEq, V> Sources<K, V> {
    pub(super) const fn new() -> Self {
        Self { running: Mutex::new(Vec::new()), next_id: AtomicU64::new(0) }
    }

    /// Adds `viewer` to the producer for `key`. When there is none, `start` gets a new source that
    /// already holds `viewer`, starts its producer thread and returns it once the producer is known
    /// to work; on an error nothing is left behind (`start` ends any thread it spawned).
    pub(super) fn join(
        &'static self,
        key: K,
        viewer: V,
        start: impl FnOnce(&Arc<Source<V>>) -> Result<JoinHandle<()>>,
    ) -> Result<Member<K, V>> {
        let mut running = lock(&self.running);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Some((_, source)) = running.iter().find(|(k, _)| *k == key) {
            lock(&source.viewers).push((id, viewer));
            return Ok(Member { sources: self, source: source.clone(), id });
        }
        let source = Arc::new(Source {
            viewers: Mutex::new(vec![(id, viewer)]),
            stop: AtomicBool::new(false),
            thread: Mutex::new(None),
        });
        match start(&source) {
            Ok(thread) => *lock(&source.thread) = Some(thread),
            Err(error) => {
                source.stop();
                return Err(error);
            }
        }
        running.push((key, source.clone()));
        Ok(Member { sources: self, source, id })
    }
}

impl<V> Source<V> {
    /// Whether the producer should end: the last viewer left, or its start failed.
    pub(super) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Asks the producer to end (its start timed out).
    pub(super) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Calls `f` for every viewer, in the order they joined. Viewers cannot join or leave meanwhile.
    pub(super) fn each(&self, mut f: impl FnMut(&mut V)) {
        for (_, viewer) in lock(&self.viewers).iter_mut() {
            f(viewer);
        }
    }
}

impl<K, V> Member<K, V> {
    /// How many viewers the producer has, this one included.
    pub(super) fn viewers(&self) -> usize {
        lock(&self.source.viewers).len()
    }
}

impl<K: 'static, V: 'static> Drop for Member<K, V> {
    fn drop(&mut self) {
        // Held to the end: nobody may start a producer for this key until the old one is gone.
        let mut running = lock(&self.sources.running);
        let (left, empty) = {
            let mut viewers = lock(&self.source.viewers);
            let left = viewers.iter().position(|(id, _)| *id == self.id).map(|i| viewers.remove(i));
            (left, viewers.is_empty())
        };
        // The viewer's own resources go outside the producer's lock.
        drop(left);
        if !empty {
            return;
        }
        running.retain(|(_, source)| !Arc::ptr_eq(source, &self.source));
        self.source.stop();
        let thread = lock(&self.source.thread).take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::AtomicUsize,
            mpsc::{self, Receiver, Sender},
        },
        thread,
        time::Duration,
    };

    use anyhow::bail;

    use super::*;

    /// A producer thread that sends `0, 1, 2, ...` to every viewer until it is stopped.
    fn counting(source: &Arc<Source<Sender<u32>>>) -> Result<JoinHandle<()>> {
        let source = source.clone();
        Ok(thread::spawn(move || {
            let mut n = 0;
            while !source.stopping() {
                source.each(|viewer| {
                    let _ = viewer.send(n);
                });
                n += 1;
                thread::sleep(Duration::from_millis(1));
            }
        }))
    }

    fn receives(rx: &Receiver<u32>) -> bool {
        rx.recv_timeout(Duration::from_secs(5)).is_ok()
    }

    #[test]
    fn viewers_of_one_monitor_share_one_producer_and_all_get_frames() {
        static SOURCES: Sources<u32, Sender<u32>> = Sources::new();
        let starts = AtomicUsize::new(0);
        let start = |source: &Arc<Source<Sender<u32>>>| {
            starts.fetch_add(1, Ordering::SeqCst);
            counting(source)
        };
        let ((tx1, rx1), (tx2, rx2), (tx3, rx3)) = (mpsc::channel(), mpsc::channel(), mpsc::channel());
        let first = SOURCES.join(7, tx1, start).unwrap();
        let second = SOURCES.join(7, tx2, start).unwrap();
        let other = SOURCES.join(8, tx3, start).unwrap();
        assert_eq!(starts.load(Ordering::SeqCst), 2, "one producer per monitor");
        assert_eq!((first.viewers(), second.viewers(), other.viewers()), (2, 2, 1));
        assert!(receives(&rx1) && receives(&rx2) && receives(&rx3));

        // The first viewer leaves; the producer keeps serving the second.
        drop(first);
        assert_eq!(second.viewers(), 1);
        while rx1.try_recv().is_ok() {}
        assert!(receives(&rx2));
        assert!(rx1.recv_timeout(Duration::from_millis(50)).is_err(), "a viewer that left gets nothing");
        drop((second, other));
        assert!(lock(&SOURCES.running).is_empty());
    }

    #[test]
    fn the_last_viewer_out_waits_for_the_producer_to_finish() {
        static SOURCES: Sources<u32, ()> = Sources::new();
        let finished = Arc::new(AtomicBool::new(false));
        let member = SOURCES
            .join(1, (), |source| {
                let (source, finished) = (source.clone(), finished.clone());
                Ok(thread::spawn(move || {
                    while !source.stopping() {
                        thread::sleep(Duration::from_millis(1));
                    }
                    // Letting go of the output takes a moment.
                    thread::sleep(Duration::from_millis(50));
                    finished.store(true, Ordering::SeqCst);
                }))
            })
            .unwrap();
        drop(member);
        assert!(finished.load(Ordering::SeqCst));
    }

    #[test]
    fn a_failed_start_leaves_nothing_behind() {
        static SOURCES: Sources<u32, ()> = Sources::new();
        let error = SOURCES.join(1, (), |_| bail!("duplicate the output: no")).err().unwrap();
        assert_eq!(error.to_string(), "duplicate the output: no");
        assert!(lock(&SOURCES.running).is_empty());
        // The next viewer tries again rather than joining a dead producer.
        let starts = AtomicUsize::new(0);
        let member = SOURCES
            .join(1, (), |source| {
                starts.fetch_add(1, Ordering::SeqCst);
                let source = source.clone();
                Ok(thread::spawn(move || {
                    while !source.stopping() {
                        thread::sleep(Duration::from_millis(1));
                    }
                }))
            })
            .unwrap();
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        drop(member);
    }

    /// DXGI's rule, modelled: starting a producer fails while an earlier one of the same monitor
    /// still holds the output (E_INVALIDARG); a producer lets go of it only as its thread ends.
    fn exclusive(
        held: &'static AtomicBool,
        letting_go: Duration,
    ) -> impl Fn(&Arc<Source<()>>) -> Result<JoinHandle<()>> + Copy {
        move |source| {
            if held.swap(true, Ordering::SeqCst) {
                bail!("the output is already duplicated");
            }
            let source = source.clone();
            Ok(thread::spawn(move || {
                while !source.stopping() {
                    thread::sleep(Duration::from_micros(200));
                }
                thread::sleep(letting_go);
                held.store(false, Ordering::SeqCst);
            }))
        }
    }

    /// A viewer that reconnects (a quality change) while the old producer is still letting go of the
    /// output waits for it instead of failing.
    #[test]
    fn a_viewer_coming_back_at_once_waits_for_the_old_producer_to_let_go() {
        static SOURCES: Sources<u32, ()> = Sources::new();
        static HELD: AtomicBool = AtomicBool::new(false);
        let start = exclusive(&HELD, Duration::from_millis(50));
        for _ in 0..3 {
            let member = SOURCES.join(3, (), start).unwrap();
            let leaving = thread::spawn(move || drop(member));
            // The old producer is letting go now.
            thread::sleep(Duration::from_millis(10));
            let again = SOURCES.join(3, (), start).expect("the new viewer waits for the old producer");
            leaving.join().unwrap();
            drop(again);
        }
    }

    /// Viewers that come and go at once, on several threads, never run into a producer still
    /// holding the output either.
    #[test]
    fn viewers_coming_and_going_never_meet_a_producer_still_holding_the_output() {
        static SOURCES: Sources<u32, ()> = Sources::new();
        static HELD: AtomicBool = AtomicBool::new(false);
        let start = exclusive(&HELD, Duration::from_micros(500));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                thread::spawn(move || {
                    for _ in 0..50 {
                        let member = SOURCES.join(3, (), start).unwrap();
                        thread::sleep(Duration::from_micros(300));
                        drop(member);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(lock(&SOURCES.running).is_empty());
        assert!(!HELD.load(Ordering::SeqCst));
    }
}
