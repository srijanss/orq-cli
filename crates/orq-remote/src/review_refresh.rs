//! Keeps open provider reviews fresh while the server runs: a round looks again at each open review
//! last looked at [`ROUND_AGE`] ago or more, and a review opened on the page is looked at when its last
//! look is [`OPENED_AGE`] old. One look runs at a time. Merged and closed reviews are final and are not
//! looked at again. What a look finds, a failure included, is saved by the observer, so a failed look
//! leaves the review unverified until the next round tries again.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orq_core::storage::StorageRoots;
use orq_core::store::Store;

/// How old a review's last look is before a round looks again.
pub const ROUND_AGE: i64 = 10 * 60;
/// How old a review's last look is before opening it on the page looks again.
pub const OPENED_AGE: i64 = 2 * 60;
/// How many opened reviews may wait for a look at once; an open past that is not queued.
pub const PENDING_LIMIT: usize = 16;
/// How often the server checks which reviews a round should look at.
pub const ROUND_EVERY: Duration = Duration::from_secs(30);

/// Looks at one run's review at its provider and saves what it shows, or why it could not.
pub trait ReviewObserver: Send + Sync {
    fn refresh(&self, run_id: &str);
}

pub struct ReviewRefresher {
    roots: StorageRoots,
    observer: Arc<dyn ReviewObserver>,
    /// Held for each look, so the provider is asked one thing at a time.
    looking: Mutex<()>,
    /// Opened reviews waiting for their look, which one worker thread makes in turn.
    pending: Mutex<Pending>,
}

#[derive(Default)]
struct Pending {
    queue: std::collections::VecDeque<String>,
    /// The queued runs and the one being looked at, so an open while it waits adds nothing.
    held: std::collections::HashSet<String>,
    worker: bool,
}

impl ReviewRefresher {
    pub fn new(roots: StorageRoots, observer: Arc<dyn ReviewObserver>) -> Arc<ReviewRefresher> {
        Arc::new(ReviewRefresher { roots, observer, looking: Mutex::new(()), pending: Mutex::new(Pending::default()) })
    }

    /// Looks at each open review last looked at [`ROUND_AGE`] or more before `now` (seconds since the
    /// epoch). Returns how many it looked at.
    pub fn round(&self, now: i64) -> usize {
        self.round_until(now, &AtomicBool::new(false))
    }

    /// A round that starts no further look once `stop` is set.
    fn round_until(&self, now: i64, stop: &AtomicBool) -> usize {
        let Ok(due) = self.due(None, now - ROUND_AGE) else { return 0 };
        due.iter().take_while(|_| !stop.load(Ordering::SeqCst)).filter(|run| self.look(run, now - ROUND_AGE, stop)).count()
    }

    /// A review opened on the page: looks at it when it is open and its last look is [`OPENED_AGE`] or
    /// more before `now`. Returns whether it did.
    pub fn opened(&self, run_id: &str, now: i64) -> bool {
        matches!(self.due(Some(run_id), now - OPENED_AGE).as_deref(), Ok([_])) && self.look(run_id, now - OPENED_AGE, &AtomicBool::new(false))
    }

    /// A review opened on the page, queued for [`ReviewRefresher::opened`] on a worker thread so nobody
    /// waits for it. Returns whether it was queued: not when it already waits, nor when
    /// [`PENDING_LIMIT`] others do.
    pub fn request(self: &Arc<Self>, run_id: &str) -> bool {
        let mut pending = self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.held.contains(run_id) || pending.held.len() >= PENDING_LIMIT {
            return false;
        }
        pending.held.insert(run_id.to_string());
        pending.queue.push_back(run_id.to_string());
        if !pending.worker {
            pending.worker = true;
            let refresher = self.clone();
            std::thread::spawn(move || refresher.work());
        }
        true
    }

    /// Looks at the queued opened reviews in turn until none is left.
    fn work(&self) {
        loop {
            let next = {
                let mut pending = self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let next = pending.queue.pop_front();
                if next.is_none() {
                    pending.worker = false;
                }
                next
            };
            let Some(run_id) = next else { return };
            self.opened(&run_id, now());
            self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).held.remove(&run_id);
        }
    }

    /// The open reviews last looked at no later than `before`; a time that cannot be read is due.
    fn due(&self, run_id: Option<&str>, before: i64) -> Result<Vec<String>, String> {
        let store = Store::open(&self.roots.database()).map_err(|err| err.to_string())?;
        let open = store.open_provider_reviews(run_id).map_err(|err| err.to_string())?;
        Ok(open.into_iter().filter(|(_, checked)| checked.is_none_or(|at| at <= before)).map(|(run, _)| run).collect())
    }

    /// Looks at `run_id` unless, by the time it is its turn, `stop` is set or another look made it fresh
    /// or it closed.
    fn look(&self, run_id: &str, before: i64, stop: &AtomicBool) -> bool {
        let _turn = self.looking.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if stop.load(Ordering::SeqCst) || !matches!(self.due(Some(run_id), before).as_deref(), Ok([_])) {
            return false;
        }
        self.observer.refresh(run_id);
        true
    }
}

/// Rounds running on their own thread, the first at once, then every `every`; dropping it stops them
/// once the look in progress, if any, ends: no other look starts.
pub struct Rounds {
    stopping: Arc<AtomicBool>,
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

pub fn start_rounds(refresher: Arc<ReviewRefresher>, every: Duration) -> Rounds {
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let stopping = Arc::new(AtomicBool::new(false));
    let flag = stopping.clone();
    let thread = std::thread::spawn(move || loop {
        refresher.round_until(now(), &flag);
        match stopped.recv_timeout(every) {
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            _ => break,
        }
    });
    Rounds { stopping, stop: Some(stop), thread: Some(thread) }
}

impl Drop for Rounds {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Seconds since the epoch.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The run a review page asks about: `/api/reviews/{run}` or `/api/projects/{id}/reviews/{run}`, with
/// any query left off.
pub fn opened_run(path: &str) -> Option<&str> {
    let path = path.split('?').next()?;
    let segments: Vec<&str> = path.strip_prefix('/')?.split('/').collect();
    match segments.as_slice() {
        ["api", "reviews", run] | ["api", "projects", _, "reviews", run] if !run.is_empty() => Some(run),
        _ => None,
    }
}
