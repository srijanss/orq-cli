//! The process groups a run has started, so that the signal which ends the run's owner ends them too.
//! Gate steps and sessions run in groups of their own, which a signal to the owner does not reach.

use std::io;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus};
use std::sync::atomic::{AtomicI32, Ordering};

/// More than a run has going at once; a group that finds no free slot would outlive a stop.
const SLOTS: usize = 32;

/// Plain atomics, because the signal handler may touch nothing else.
static GROUPS: [AtomicI32; SLOTS] = [const { AtomicI32::new(0) }; SLOTS];

/// A running child leading a process group of its own, which ends with the run's owner. The child can
/// only be reaped through `reap`, which stops tracking it first: once reaped, its id may name another
/// process's group. Dropping it without reaping gives up on it: its group is ended and it is reaped.
pub(crate) struct TrackedChild {
    child: Child,
    slot: Option<&'static AtomicI32>,
    reaped: bool,
}

/// Starts `command`, which must put the child in a group of its own.
pub(crate) fn spawn(command: &mut Command) -> io::Result<TrackedChild> {
    let child = command.spawn()?;
    let group = child.id() as i32;
    let slot = GROUPS.iter().find(|slot| slot.compare_exchange(0, group, Ordering::SeqCst, Ordering::SeqCst).is_ok());
    Ok(TrackedChild { child, slot, reaped: false })
}

impl TrackedChild {
    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }

    pub(crate) fn take_output(&mut self) -> (Option<ChildStdout>, Option<ChildStderr>) {
        (self.child.stdout.take(), self.child.stderr.take())
    }

    /// Whether the leader has exited, without reaping it.
    pub(crate) fn has_exited(&self) -> io::Result<bool> {
        crate::terminal_session::has_exited(&self.child)
    }

    /// Blocks until the leader has exited, without reaping it.
    pub(crate) fn wait_for_exit(&self) -> io::Result<()> {
        loop {
            // Safety: an all-zero siginfo is valid for waitid to fill in.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // Safety: `info` is valid for the call; the child is ours and not yet reaped.
            let status = unsafe { libc::waitid(libc::P_PID, self.id() as libc::id_t, &mut info, libc::WEXITED | libc::WNOWAIT) };
            if status == 0 {
                return Ok(());
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }

    /// Kills everything still in the group.
    pub(crate) fn end_group(&self) {
        // Safety: plain system call; the leader is unreaped, so its id names only this group.
        unsafe { libc::killpg(self.id() as libc::pid_t, libc::SIGKILL) };
    }

    /// Stops tracking the group and reaps its leader, waiting for the leader if it is still running.
    pub(crate) fn reap(mut self) -> io::Result<ExitStatus> {
        self.untrack();
        self.reaped = true;
        self.child.wait()
    }

    fn untrack(&mut self) {
        if let Some(slot) = self.slot.take() {
            slot.store(0, Ordering::SeqCst);
        }
    }
}

impl Drop for TrackedChild {
    fn drop(&mut self) {
        if !self.reaped {
            self.end_group();
            self.untrack();
            let _ = self.child.wait();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    fn is_tracked(pid: u32) -> bool {
        GROUPS.iter().any(|slot| slot.load(Ordering::SeqCst) == pid as i32)
    }

    #[test]
    fn a_finished_child_stays_tracked_and_keeps_its_id_until_it_is_reaped() {
        let child = spawn(Command::new("true").process_group(0)).unwrap();
        let pid = child.id();

        child.wait_for_exit().unwrap();

        assert!(is_tracked(pid));
        // A reaped process's id is free to be reused; signal 0 only checks that the id is still its own.
        // Safety: plain system call.
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, 0, "waiting for the exit reaped it");

        assert!(child.reap().unwrap().success());

        assert!(!is_tracked(pid));
    }

    fn running(pid: i32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            let state = stat.rfind(')').and_then(|at| stat[at + 1..].split_whitespace().next().map(str::to_string));
            !matches!(state.as_deref(), Some("Z" | "X" | "x"))
        })
    }

    fn scratch_file(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("orq-child-groups-{}-{tag}", std::process::id()))
    }

    /// Waits for a program to write the id of what it started.
    fn written_pid(file: &std::path::Path) -> i32 {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(pid) = std::fs::read_to_string(file).ok().and_then(|text| text.trim().parse().ok()) {
                let _ = std::fs::remove_file(file);
                return pid;
            }
            assert!(std::time::Instant::now() < until, "nothing was written to {}", file.display());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn gone_soon(pid: i32) -> bool {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while running(pid) {
            if std::time::Instant::now() >= until {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        true
    }

    /// An early return, such as an error while watching the child, must not leave the group running
    /// with nothing tracking it any more.
    #[test]
    fn a_child_given_up_on_ends_with_its_group_and_is_reaped() {
        let file = scratch_file("given-up");
        let script = format!("sleep 300 & echo $! > '{}'; wait", file.display());
        let child = spawn(Command::new("sh").args(["-c", &script]).process_group(0)).unwrap();
        let leader = child.id() as i32;
        let started = written_pid(&file);

        drop(child);

        let gone = gone_soon(started);
        if !gone {
            // Safety: plain system call, so the test leaves nothing behind.
            unsafe { libc::kill(started, libc::SIGKILL) };
        }
        assert!(gone, "what the child started outlived it");
        // Safety: a null status is allowed; the leader is this process's child unless it was reaped.
        let reaped = unsafe { libc::waitpid(leader, std::ptr::null_mut(), libc::WNOHANG) } == -1;
        assert!(reaped, "the leader was left unreaped");
    }
}
