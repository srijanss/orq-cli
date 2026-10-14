use std::path::Path;
use std::process::Command;

use orq_test_support::GitFixture;

/// Runs `orq ui` on a pseudo-terminal, sends Esc, and returns
/// (exit status, everything the popup drew). Uses python3's `pty` module so
/// the child gets a real terminal without any extra crate.
fn run_in_pty(pane_dir: &Path) -> (i32, String) {
    const DRIVER: &str = r#"
import fcntl, os, pty, select, struct, sys, termios, time
pid, fd = pty.fork()
if pid == 0:
    fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
    os.execv(sys.argv[1], [sys.argv[1], "ui"])
# Keep our own slave descriptor so the terminal's attributes can be compared
# before and after the child runs.
slave = os.open(os.ptsname(fd), os.O_RDWR | os.O_NOCTTY)
before = termios.tcgetattr(slave)
out = b""
sent = False
deadline = time.time() + 15
status = None
while time.time() < deadline:
    done, st = os.waitpid(pid, os.WNOHANG)
    if done:
        status = st
        # Drain what the child wrote before it exited.
        while select.select([fd], [], [], 0)[0]:
            try:
                chunk = os.read(fd, 4096)
            except OSError:
                break
            if not chunk:
                break
            out += chunk
        break
    r, _, _ = select.select([fd], [], [], 0.2)
    if r:
        try:
            chunk = os.read(fd, 4096)
        except OSError:
            break
        if not chunk:
            break
        out += chunk
    if not sent and b"repo" in out:
        os.write(fd, b"\x1b")
        sent = True
import signal
end = time.time() + 5
while status is None and time.time() < end:
    done, st = os.waitpid(pid, os.WNOHANG)
    if done:
        status = st
    else:
        time.sleep(0.05)
if status is None:
    # Never block on a wedged child: kill it and report the output so far.
    os.kill(pid, signal.SIGKILL)
    os.waitpid(pid, 0)
    sys.stdout.buffer.write(out)
    sys.stdout.buffer.write(b"\n[driver: orq did not exit and was killed]")
    sys.exit(124)
after = termios.tcgetattr(slave)
sys.stdout.buffer.write(out)
sys.stdout.buffer.write(b"\n[termios-restored]" if after[3] == before[3] else b"\n[termios-differs]")
sys.exit(os.waitstatus_to_exitcode(status))
"#;
    let output = Command::new("python3")
        .args(["-I", "-c", DRIVER, env!("CARGO_BIN_EXE_orq")])
        .env("HERDR_ACTIVE_PANE_CWD", pane_dir)
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .output()
        .unwrap();
    (
        output.status.code().unwrap(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn interactive_ui_draws_the_project_and_exits_cleanly_on_esc() {
    let repo = GitFixture::new();
    let root = repo.root().canonicalize().unwrap();

    let (code, drawn) = run_in_pty(repo.nested_dir());

    assert_eq!(code, 0, "{drawn}");
    assert!(drawn.contains(root.to_str().unwrap()), "{drawn}");
    // Raw mode is undone (echo and canonical input are back).
    assert!(drawn.ends_with("[termios-restored]"), "{drawn:?}");
    // The terminal is put back: mouse capture off and the alternate screen
    // left, after they were turned on.
    let position = |needle: &str| drawn.find(needle).unwrap_or_else(|| panic!("{needle:?} missing: {drawn:?}"));
    assert!(position("\x1b[?1049h") < position("\x1b[?1049l"), "{drawn:?}");
    assert!(position("\x1b[?1000h") < position("\x1b[?1000l"), "{drawn:?}");
}
