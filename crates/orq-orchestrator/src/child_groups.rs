//! The process groups a run has started, so that the signal which ends the run's owner ends them too.
//! Gate steps and sessions run in groups of their own, which a signal to the owner does not reach.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

/// More than a run has going at once; a group that finds no free slot would outlive a stop.
const SLOTS: usize = 32;

/// Plain atomics, because the signal handler may touch nothing else.
static GROUPS: [AtomicI32; SLOTS] = [const { AtomicI32::new(0) }; SLOTS];

/// How many threads are between starting a child and tracking it. The signal handler waits for them,
/// because it cannot end a group nothing has tracked yet.
static STARTING: AtomicUsize = AtomicUsize::new(0);

/// Set by the signal handler before it reads `STARTING`, and read by a start after it raises
/// `STARTING`: so either the handler waits for that start, or that start sees this and never happens.
static ENDING: AtomicBool = AtomicBool::new(false);

/// How long the signal handler waits for a child that is starting, so a stuck start cannot keep the
/// process from ending.
const START_WAIT_TICKS: usize = 5_000;
const START_WAIT_TICK_NANOS: libc::c_long = 1_000_000;

const END_SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];

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
    spawn_with(command, |_| {})
}

/// `spawn`, running `before_tracking` between the start and the tracking, so a test can stop the
/// process at exactly that point.
fn spawn_with(command: &mut Command, before_tracking: impl FnOnce(&Child)) -> io::Result<TrackedChild> {
    let start = Start::begin();
    let old_mask = start.old_mask;
    // Safety: the closure only makes async-signal-safe calls, on a copy of the mask.
    unsafe {
        command.pre_exec(move || {
            // The handler is this process's and must not run in the child, which the mask let through
            // now would allow. A signal being ignored stays ignored, as it would across the exec.
            for signal in END_SIGNALS {
                if libc::signal(signal, libc::SIG_DFL) == libc::SIG_IGN {
                    libc::signal(signal, libc::SIG_IGN);
                }
            }
            libc::pthread_sigmask(libc::SIG_SETMASK, &old_mask, std::ptr::null_mut());
            Ok(())
        });
    }
    let child = command.spawn()?;
    before_tracking(&child);
    let group = child.id() as i32;
    let slot = GROUPS.iter().find(|slot| slot.compare_exchange(0, group, Ordering::SeqCst, Ordering::SeqCst).is_ok());
    Ok(TrackedChild { child, slot, reaped: false })
}

/// The time between starting a child and tracking it, during which this thread does not take the
/// signals that end the process: the handler running on this thread would wait for this very start.
struct Start {
    old_mask: libc::sigset_t,
}

impl Start {
    fn begin() -> Start {
        // Safety: an all-zero sigset_t is valid storage, and these calls only fill in what they are given.
        let old_mask = unsafe {
            let mut blocked: libc::sigset_t = std::mem::zeroed();
            let mut old_mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut blocked);
            for signal in END_SIGNALS {
                libc::sigaddset(&mut blocked, signal);
            }
            libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut old_mask);
            old_mask
        };
        STARTING.fetch_add(1, Ordering::SeqCst);
        if ENDING.load(Ordering::SeqCst) {
            // The handler is ending this process and may not have seen this start.
            STARTING.fetch_sub(1, Ordering::SeqCst);
            loop {
                std::thread::park();
            }
        }
        Start { old_mask }
    }
}

impl Drop for Start {
    fn drop(&mut self) {
        // In this order: a signal let through now must find nothing to wait for.
        STARTING.fetch_sub(1, Ordering::SeqCst);
        // Safety: the mask was filled in by `begin`.
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &self.old_mask, std::ptr::null_mut()) };
    }
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

    /// Stops tracking the group, in the order that makes reaping it afterwards safe. The handler sets
    /// `ENDING` before it reads the slots, so if it read this id, this thread sees `ENDING` here and
    /// never goes on to free the id while the handler is about to signal it.
    fn untrack(&mut self) {
        if let Some(slot) = self.slot.take() {
            slot.store(0, Ordering::SeqCst);
        }
        if ENDING.load(Ordering::SeqCst) {
            loop {
                std::thread::park();
            }
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
    for signal in END_SIGNALS {
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
    ENDING.store(true, Ordering::SeqCst);
    let tick = libc::timespec { tv_sec: 0, tv_nsec: START_WAIT_TICK_NANOS };
    for _ in 0..START_WAIT_TICKS {
        if STARTING.load(Ordering::SeqCst) == 0 {
            break;
        }
        // Safety: async-signal-safe, and `tick` is valid for the call.
        unsafe { libc::nanosleep(&tick, std::ptr::null_mut()) };
    }
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
        orq_test_support::process_temp_dir().join(format!("orq-child-groups-{}-{tag}", std::process::id()))
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

    /// The signals are held back only while the child starts; children inherit what is held back.
    #[test]
    fn a_started_child_takes_signals_as_usual() {
        use std::os::unix::process::ExitStatusExt;
        let child = spawn(Command::new("sleep").arg("300").process_group(0)).unwrap();

        // Safety: plain system call on our own unreaped child.
        unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };

        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !child.has_exited().unwrap() {
            // Dropping the child on this failure ends it.
            assert!(std::time::Instant::now() < until, "the child did not take the signal");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(child.reap().unwrap().signal(), Some(libc::SIGTERM));
    }

    const PID_FILE_VARIABLE: &str = "ORQ_TEST_CHILD_PID_FILE";

    /// Not a check of its own: `a_stop_while_a_child_is_starting_still_ends_it` runs it in a process
    /// of its own, which the stop then ends.
    #[test]
    #[ignore = "only run by a_stop_while_a_child_is_starting_still_ends_it"]
    fn stopped_while_starting_a_child() {
        let Ok(file) = std::env::var(PID_FILE_VARIABLE) else { return };
        end_children_with_this_process();

        let _child = spawn_with(Command::new("sleep").arg("300").process_group(0), |child| {
            std::fs::write(&file, child.id().to_string()).unwrap();
            // Safety: plain system calls; the stop arrives after the start and before the tracking.
            unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
            std::thread::sleep(std::time::Duration::from_millis(200));
        });
    }

    #[test]
    fn a_stop_while_a_child_is_starting_still_ends_it() {
        use std::os::unix::process::ExitStatusExt;
        let file = scratch_file("starting");

        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_groups::tests::stopped_while_starting_a_child", "--ignored"])
            .env(PID_FILE_VARIABLE, &file)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();

        let started = written_pid(&file);
        let gone = gone_soon(started);
        if !gone {
            // Safety: plain system call, so the test leaves nothing behind.
            unsafe { libc::kill(started, libc::SIGKILL) };
        }
        assert_eq!(status.signal(), Some(libc::SIGTERM), "the stop ended the process");
        assert!(gone, "the child outlived the stop");
    }

    const ENDING_VARIABLE: &str = "ORQ_TEST_REAP_WHILE_ENDING";
    const NOTHING_WAS_REAPED: i32 = 7;

    /// Whether the process is still an exited child nothing has reaped.
    fn unreaped(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            let state = stat.rfind(')').and_then(|at| stat[at + 1..].split_whitespace().next().map(str::to_string));
            state.as_deref() == Some("Z")
        })
    }

    /// Not a check of its own: `a_child_is_not_reaped_once_the_process_is_ending` runs it in a process
    /// of its own, because `ENDING` never goes back.
    #[test]
    #[ignore = "only run by a_child_is_not_reaped_once_the_process_is_ending"]
    fn reaping_while_ending() {
        if std::env::var_os(ENDING_VARIABLE).is_none() {
            return;
        }
        let exited = || {
            let child = spawn(Command::new("true").process_group(0)).unwrap();
            child.wait_for_exit().unwrap();
            child
        };
        let (reaped_on_purpose, given_up_on) = (exited(), exited());
        let pids = [reaped_on_purpose.id(), given_up_on.id()];
        ENDING.store(true, Ordering::SeqCst);

        std::thread::spawn(move || reaped_on_purpose.reap());
        std::thread::spawn(move || drop(given_up_on));
        std::thread::sleep(std::time::Duration::from_millis(300));

        let both_unreaped = pids.iter().all(|pid| unreaped(*pid));
        std::process::exit(if both_unreaped { NOTHING_WAS_REAPED } else { 1 });
    }

    /// The signal handler may have read a group's id just before it stopped being tracked and be about
    /// to signal it. That id must keep naming that group until the process has ended.
    #[test]
    fn a_child_is_not_reaped_once_the_process_is_ending() {
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_groups::tests::reaping_while_ending", "--ignored"])
            .env(ENDING_VARIABLE, "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();

        assert_eq!(status.code(), Some(NOTHING_WAS_REAPED), "a child was reaped while the process was ending");
    }
}
