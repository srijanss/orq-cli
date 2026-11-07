//! An agent session given orq's terminal is closed by orq once it says it is finished, so it never
//! gets to undo what it did to the terminal. orq gives the terminal back as it found it: Enter ends a
//! line again (the pane's "press Enter to close" prompt works), and the agent's screen does not stay
//! in the pane under orq's own progress lines.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{run_implement_stage, ImplementSession, RoleProfile, RunRecord, RunStore};

/// Set in the copy of this test binary that runs on the pseudo-terminal: the folder it works in.
const CHILD: &str = "ORQ_HANDBACK_CHILD";
/// What the session draws, so the driver can tell its output from what orq writes after it.
const DREW: &str = "SESSION-DREW";

/// Runs this test binary's `test` on a new pseudo-terminal it controls, with `CHILD` set to `root`.
/// Returns everything written to the terminal, then a line saying whether its settings afterwards
/// are the ones it had before.
fn run_on_terminal(test: &str, root: &Path) -> String {
    const DRIVER: &str = r#"
import fcntl, os, pty, select, signal, struct, sys, termios, time
fd, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
before = termios.tcgetattr(slave)
pid = os.fork()
if pid == 0:
    os.setsid()
    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
    for n in (0, 1, 2):
        os.dup2(slave, n)
    os.close(fd)
    os.close(slave)
    os.execv(sys.argv[1], [sys.argv[1], sys.argv[2], "--exact", "--nocapture", "--test-threads=1"])
out = b""
status = None
deadline = time.time() + 30
while time.time() < deadline:
    r, _, _ = select.select([fd], [], [], 0.1)
    if r:
        try:
            chunk = os.read(fd, 4096)
        except OSError:
            chunk = b""
        out += chunk
    done, st = os.waitpid(pid, os.WNOHANG)
    if done:
        status = st
        while select.select([fd], [], [], 0)[0]:
            try:
                chunk = os.read(fd, 4096)
            except OSError:
                break
            if not chunk:
                break
            out += chunk
        break
if status is None:
    os.kill(pid, signal.SIGKILL)
    os.waitpid(pid, 0)
    sys.stdout.buffer.write(out + b"\n[driver: the test did not finish]")
    sys.exit(124)
after = termios.tcgetattr(slave)
sys.stdout.buffer.write(out)
if after == before:
    sys.stdout.buffer.write(b"\n[termios-restored]")
else:
    sys.stdout.buffer.write(("\n[termios-differs: before=%r after=%r]" % (before, after)).encode())
"#;
    let exe = std::env::current_exe().unwrap();
    let output = Command::new("python3")
        .args(["-I", "-c", DRIVER])
        .arg(exe)
        .arg(test)
        .env(CHILD, root)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn store_with_run(root: &Path) -> RunStore {
    fs::create_dir_all(root.join("worktree")).unwrap();
    let store = RunStore::new(&root.join("state"));
    let mut profiles = BTreeMap::new();
    profiles.insert(
        "implementer".to_string(),
        RoleProfile {
            harness: "claude".to_string(),
            model: "opus".to_string(),
            thinking: None,
        },
    );
    store
        .create(&RunRecord {
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            remote: "origin".to_string(),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
            branch: "fix--COM-02".to_string(),
            worktree: PathBuf::from("worktree"),
            repository: String::new(),
            profiles,
        })
        .unwrap();
    store
}

/// On the pseudo-terminal: a session that takes the terminal the way an agent's own screen does
/// (raw input, a hidden cursor, a screen of its own), draws, says it is finished and keeps running,
/// so orq has to close it.
fn run_an_agent_like_session_closed_by_its_marker(root: &Path) {
    run_an_agent_like_session(root, true);
}

/// `run_an_agent_like_session_closed_by_its_marker`, given orq's terminal only when `terminal`.
fn run_an_agent_like_session(root: &Path, terminal: bool) {
    let marker = root.join("worktree/done.marker");
    let script = format!(
        "stty raw -echo; printf '\\033[?1049h\\033[?25l{DREW}'; touch {}; sleep 30",
        marker.display()
    );
    let outcome = run_session(root, "sh", &["-c".to_string(), script], terminal).unwrap();
    assert!(outcome.success, "{outcome:?}");
}

/// Runs `program` as a terminal-attached session of run-1 whose finish marker is
/// `worktree/done.marker`.
fn run_terminal_session(
    root: &Path,
    program: &str,
    args: &[String],
) -> Result<orq_orchestrator::ImplementOutcome, orq_orchestrator::StoreError> {
    run_session(root, program, args, true)
}

/// `run_terminal_session`, given orq's terminal only when `terminal`.
fn run_session(
    root: &Path,
    program: &str,
    args: &[String],
    terminal: bool,
) -> Result<orq_orchestrator::ImplementOutcome, orq_orchestrator::StoreError> {
    let store = store_with_run(root);
    let marker = root.join("worktree/done.marker");
    let session = ImplementSession {
        program,
        args,
        env: &[],
        cwd: &root.join("worktree"),
        session_id: None,
        log: &root.join("implement.log"),
        terminal,
        exit_marker: Some(&marker),
    };
    run_implement_stage(&store, "run-1", &session)
}

/// What a session that ends by itself prints last, so the driver can tell its output from what orq
/// writes after it.
const LAST_WORDS: &str = "AGENT-GAVE-UP";

/// The screen reset orq writes after closing a session, in part: leaving the session's own screen
/// and clearing the screen.
const RESET_PARTS: [&str; 2] = ["\x1b[?1049l", "\x1b[2J"];

fn fresh_root(tag: &str) -> PathBuf {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-handback-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn a_terminal_session_closed_by_orq_leaves_the_terminal_settings_as_they_were_so_enter_ends_a_line()
{
    if let Some(root) = std::env::var_os(CHILD) {
        return run_an_agent_like_session_closed_by_its_marker(Path::new(&root));
    }
    let root = fresh_root("termios");
    let screen = run_on_terminal(
        "a_terminal_session_closed_by_orq_leaves_the_terminal_settings_as_they_were_so_enter_ends_a_line",
        &root,
    );

    assert!(
        screen.contains(DREW),
        "the session never ran on the terminal: {screen:?}"
    );
    assert!(
        screen.contains("1 passed"),
        "the session stage failed on the terminal: {screen:?}"
    );
    assert!(screen.ends_with("[termios-restored]"), "{screen:?}");
}

#[test]
fn a_terminal_session_closed_by_orq_has_its_screen_modes_undone_and_its_screen_cleared() {
    if let Some(root) = std::env::var_os(CHILD) {
        return run_an_agent_like_session_closed_by_its_marker(Path::new(&root));
    }
    let root = fresh_root("screen");
    let screen = run_on_terminal(
        "a_terminal_session_closed_by_orq_has_its_screen_modes_undone_and_its_screen_cleared",
        &root,
    );

    let (_, after) = screen
        .split_once(DREW)
        .unwrap_or_else(|| panic!("the session never ran on the terminal: {screen:?}"));
    for (what, sequence) in [
        ("leave the session's own screen", "\x1b[?1049l"),
        ("show the cursor", "\x1b[?25h"),
        ("clear the screen", "\x1b[2J"),
    ] {
        assert!(
            after.contains(sequence),
            "orq did not {what} after the session: {after:?}"
        );
    }
}

#[test]
fn a_terminal_session_that_cannot_start_leaves_the_screen_and_terminal_settings_alone() {
    if let Some(root) = std::env::var_os(CHILD) {
        let _ = run_terminal_session(Path::new(&root), "orq-handback-no-such-agent", &[]);
        return;
    }
    let root = fresh_root("no-start");
    let screen = run_on_terminal(
        "a_terminal_session_that_cannot_start_leaves_the_screen_and_terminal_settings_alone",
        &root,
    );

    assert!(screen.contains("1 passed"), "{screen:?}");
    for part in RESET_PARTS {
        assert!(
            !screen.contains(part),
            "orq reset a screen no session used: {screen:?}"
        );
    }
    assert!(screen.ends_with("[termios-restored]"), "{screen:?}");
}

#[test]
fn a_terminal_session_that_ends_by_itself_keeps_what_it_printed_on_the_screen() {
    if let Some(root) = std::env::var_os(CHILD) {
        let script = format!("printf '{LAST_WORDS}'; exit 3");
        let outcome =
            run_terminal_session(Path::new(&root), "sh", &["-c".to_string(), script]).unwrap();
        assert!(!outcome.success, "{outcome:?}");
        return;
    }
    let root = fresh_root("own-end");
    let screen = run_on_terminal(
        "a_terminal_session_that_ends_by_itself_keeps_what_it_printed_on_the_screen",
        &root,
    );

    assert!(screen.contains("1 passed"), "{screen:?}");
    let (_, after) = screen
        .split_once(LAST_WORDS)
        .unwrap_or_else(|| panic!("the session never ran on the terminal: {screen:?}"));
    for part in RESET_PARTS {
        assert!(
            !after.contains(part),
            "orq cleared what the session printed: {after:?}"
        );
    }
    assert!(screen.ends_with("[termios-restored]"), "{screen:?}");
}

#[test]
fn a_terminal_session_killed_by_a_signal_has_its_screen_modes_undone_and_its_screen_cleared() {
    if let Some(root) = std::env::var_os(CHILD) {
        let script = format!("stty raw -echo; printf '\\033[?1049h\\033[?25l{DREW}'; kill -9 $$");
        let outcome =
            run_terminal_session(Path::new(&root), "sh", &["-c".to_string(), script]).unwrap();
        assert!(!outcome.success, "{outcome:?}");
        return;
    }
    let root = fresh_root("signal");
    let screen = run_on_terminal(
        "a_terminal_session_killed_by_a_signal_has_its_screen_modes_undone_and_its_screen_cleared",
        &root,
    );

    assert!(screen.contains("1 passed"), "{screen:?}");
    let (_, after) = screen
        .split_once(DREW)
        .unwrap_or_else(|| panic!("the session never ran on the terminal: {screen:?}"));
    for (what, sequence) in [
        ("leave the session's own screen", "\x1b[?1049l"),
        ("show the cursor", "\x1b[?25h"),
        ("clear the screen", "\x1b[2J"),
    ] {
        assert!(
            after.contains(sequence),
            "orq did not {what} after the session: {after:?}"
        );
    }
    assert!(screen.ends_with("[termios-restored]"), "{screen:?}");
}

#[test]
fn a_session_not_given_the_terminal_leaves_the_screen_and_terminal_settings_alone() {
    if let Some(root) = std::env::var_os(CHILD) {
        return run_an_agent_like_session(Path::new(&root), false);
    }
    let root = fresh_root("no-terminal");
    let screen = run_on_terminal(
        "a_session_not_given_the_terminal_leaves_the_screen_and_terminal_settings_alone",
        &root,
    );

    assert!(screen.contains("1 passed"), "{screen:?}");
    for part in RESET_PARTS {
        assert!(
            !screen.contains(part),
            "orq reset a screen no session used: {screen:?}"
        );
    }
    assert!(screen.ends_with("[termios-restored]"), "{screen:?}");
}

#[test]
fn a_terminal_session_closed_by_orq_writes_no_screen_reset_into_a_redirected_stdout() {
    if let Some(root) = std::env::var_os(CHILD) {
        let root = Path::new(&root);
        let file = fs::File::create(root.join("stdout.txt")).unwrap();
        // SAFETY: points stdout at a file this process owns, before anything else uses it.
        assert_eq!(
            unsafe { libc::dup2(std::os::fd::AsRawFd::as_raw_fd(&file), 1) },
            1
        );
        return run_an_agent_like_session_closed_by_its_marker(root);
    }
    let root = fresh_root("redirected");
    let screen = run_on_terminal(
        "a_terminal_session_closed_by_orq_writes_no_screen_reset_into_a_redirected_stdout",
        &root,
    );
    let stdout = fs::read_to_string(root.join("stdout.txt")).unwrap();

    assert!(stdout.contains("1 passed"), "{screen:?} {stdout:?}");
    let (_, after) = stdout
        .split_once(DREW)
        .unwrap_or_else(|| panic!("the session never wrote to stdout: {stdout:?}"));
    for part in RESET_PARTS {
        assert!(
            !after.contains(part),
            "orq wrote a screen reset into a file: {after:?}"
        );
    }
    assert!(screen.ends_with("[termios-restored]"), "{screen:?}");
}
