//! Running a program that insists on a terminal when nobody is there to look at it.

use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// How long a program asked to stop has before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(2);
const POLL_MS: u16 = 100;

/// Runs `command` under a new terminal of its own, reading and discarding what it shows, until it exits.
/// One still running is stopped, with everything it started, `linger` after `done_marker` appears or at
/// `limit`, whichever is first. Nothing is ever typed into the terminal.
pub fn run_in_terminal(mut command: Command, done_marker: &Path, limit: Duration, linger: Duration) -> io::Result<()> {
    let (mut screen, terminal) = open_terminal()?;
    command.stdin(terminal.try_clone()?).stdout(terminal.try_clone()?).stderr(terminal);
    // Safety: `setsid` and `ioctl` are async-signal-safe, and nothing else happens between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    // The command holds this end of the terminal, which only the program may keep open.
    drop(command);

    let mut deadline = Instant::now() + limit;
    let mut marked = false;
    while Instant::now() < deadline {
        discard_output(&mut screen);
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if !marked && done_marker.exists() {
            marked = true;
            deadline = deadline.min(Instant::now() + linger);
        }
    }
    stop(&mut child, &mut screen)
}

fn stop(child: &mut Child, screen: &mut File) -> io::Result<()> {
    // The program leads its own session (see `setsid`), so its id is also its group's.
    let group = child.id() as libc::pid_t;
    // Safety: plain system call; the child is not yet reaped, so the id still names its group.
    unsafe { libc::killpg(group, libc::SIGTERM) };
    let grace = Instant::now() + STOP_GRACE;
    while Instant::now() < grace {
        discard_output(screen);
        if child.try_wait()?.is_some() {
            return Ok(());
        }
    }
    // Safety: as above.
    unsafe { libc::killpg(group, libc::SIGKILL) };
    child.wait().map(|_| ())
}

/// Waits a moment for output and reads what came, since a program with a full screen buffer stops.
fn discard_output(screen: &mut File) {
    let mut wanted = libc::pollfd { fd: screen.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // Safety: `wanted` is one valid pollfd for the length of the call.
    let ready = unsafe { libc::poll(&mut wanted, 1, i32::from(POLL_MS)) };
    let mut buffer = [0u8; 16 * 1024];
    // Reading fails once the program has closed its end, and poll then no longer waits.
    if ready > 0 && (wanted.revents & libc::POLLIN == 0 || screen.read(&mut buffer).is_err()) {
        std::thread::sleep(Duration::from_millis(u64::from(POLL_MS)));
    }
}

/// A new terminal: the end to read the screen from, and the end for the program.
fn open_terminal() -> io::Result<(File, OwnedFd)> {
    let (mut screen, mut terminal) = (-1, -1);
    let mut size = libc::winsize { ws_row: 50, ws_col: 160, ws_xpixel: 0, ws_ypixel: 0 };
    // Safety: all pointers are valid for the call; a null name and termios are allowed.
    if unsafe { libc::openpty(&mut screen, &mut terminal, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // Safety: openpty has just returned these descriptors, owned by nothing else.
    let (screen, terminal) = unsafe { (OwnedFd::from_raw_fd(screen), OwnedFd::from_raw_fd(terminal)) };
    for fd in [&screen, &terminal] {
        // Safety: plain system call on a descriptor owned here. Closed on exec, so no other program
        // this process starts keeps the terminal open; the program's own copies are made for it by spawn.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok((File::from(screen), terminal))
}
