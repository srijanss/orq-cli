//! The process groups a run has started, so that the signal which ends the run's owner ends them too.
//! Gate steps and sessions run in groups of their own, which a signal to the owner does not reach.

use std::sync::atomic::{AtomicI32, Ordering};

/// More than a run has going at once; a group that finds no free slot would outlive a stop.
const SLOTS: usize = 32;

/// Plain atomics, because the signal handler may touch nothing else.
static GROUPS: [AtomicI32; SLOTS] = [const { AtomicI32::new(0) }; SLOTS];

/// A process group that ends with the run's owner until this is dropped. Drop it before reaping the
/// group's leader: after that its id may name another process's group.
pub(crate) struct TrackedGroup(Option<&'static AtomicI32>);

pub(crate) fn track(group: u32) -> TrackedGroup {
    let group = group as i32;
    TrackedGroup(GROUPS.iter().find(|slot| slot.compare_exchange(0, group, Ordering::SeqCst, Ordering::SeqCst).is_ok()))
}

impl Drop for TrackedGroup {
    fn drop(&mut self) {
        if let Some(slot) = self.0 {
            slot.store(0, Ordering::SeqCst);
        }
    }
}

/// Make this process, the owner of a run, end the groups it tracks when it is told to stop (SIGTERM),
/// interrupted (SIGINT) or loses its terminal (SIGHUP). A signal it was started ignoring stays ignored.
pub fn end_children_with_this_process() {
    let handler = end_groups_and_die as extern "C" fn(libc::c_int);
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // Safety: the handler only makes async-signal-safe calls and touches only atomics.
        unsafe {
            if libc::signal(signal, handler as libc::sighandler_t) == libc::SIG_IGN {
                libc::signal(signal, libc::SIG_IGN);
            }
        }
    }
}

/// SIGKILL, because a group that ignored a gentler signal would outlive the run. The signal is then
/// raised again with its default action, so this process ends the way it would have without a handler.
extern "C" fn end_groups_and_die(signal: libc::c_int) {
    for slot in &GROUPS {
        let group = slot.load(Ordering::SeqCst);
        if group != 0 {
            // Safety: async-signal-safe; a tracked group's leader has not been reaped.
            unsafe { libc::killpg(group, libc::SIGKILL) };
        }
    }
    // Safety: async-signal-safe; the signal is blocked until this handler returns.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}
