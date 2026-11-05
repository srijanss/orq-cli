//! `orq run` started in a terminal hands that terminal to the implement session, so Claude runs
//! interactively; without a terminal (a detached runner, a pipe) the session stays headless.

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const PR_URL: &str = "https://github.example/o/r/pull/7";

struct World {
    repo: GitFixture,
    _remote: PathBuf,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    bin: PathBuf,
    seen: PathBuf,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A repository with a pushed `main` and a pending COM-01, and a `claude` that writes whether its
/// stdin was a terminal to `seen` before it runs `agent`.
fn world(tag: &str, agent: &FakeAgent) -> World {
    let root = orq_test_support::process_temp_dir().join(format!("orq-run-tty-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config/orq")).unwrap();
    std::fs::create_dir_all(root.join("state")).unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let ssh = root.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let seen = root.join("seen");
    let claude = root.join("bin/claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nif [ -t 0 ] && [ -t 1 ]; then t=tty; else t=none; fi\necho \"$t $PWD\" >> '{seen}'\nexec '{agent}' \"$@\"\n",
            seen = seen.display(),
            agent = agent.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    World { repo, _remote: remote, state: root.join("state"), config: root.join("config"), ssh, bin: root.join("bin"), seen }
}

fn github() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build()
}

fn command(w: &World, provider: &FakeProvider, program: &str) -> Command {
    let mut dirs = vec![w.bin.clone(), provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut command = Command::new(program);
    command
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD");
    command
}

fn run_args(w: &World) -> Vec<String> {
    let repo = w.repo.root().display().to_string();
    ["run", "--repo", &repo, "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"]
        .iter()
        .map(|arg| arg.to_string())
        .collect()
}

/// Runs `orq <args>` on a fresh pseudo-terminal as its controlling terminal; the exit code and
/// everything written to the terminal.
fn run_in_pty(w: &World, provider: &FakeProvider, args: &[String]) -> (i32, String) {
    const DRIVER: &str = r#"
import fcntl, os, pty, select, signal, struct, sys, termios, time
fd, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
pid = os.fork()
if pid == 0:
    os.setsid()
    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
    for n in (0, 1, 2):
        os.dup2(slave, n)
    os.close(fd)
    os.close(slave)
    os.execv(sys.argv[1], sys.argv[1:])
os.close(slave)
out = b""
status = None
deadline = time.time() + 60
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
        break
if status is None:
    os.kill(pid, signal.SIGKILL)
    os.waitpid(pid, 0)
    sys.stdout.buffer.write(out + b"\n[driver: orq did not exit and was killed]")
    sys.exit(124)
sys.stdout.buffer.write(out)
sys.exit(os.waitstatus_to_exitcode(status))
"#;
    let output = command(w, provider, "python3")
        .args(["-I", "-c", DRIVER, env!("CARGO_BIN_EXE_orq")])
        .args(args)
        .output()
        .unwrap();
    (output.status.code().unwrap_or(-1), String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether the implement session, the first `claude` started in the run's worktree, had a
/// terminal: "tty" or "none". Later headless calls (the review draft, the push notification) are
/// left out.
fn seen(w: &World) -> String {
    let calls = std::fs::read_to_string(&w.seen).unwrap_or_default();
    calls
        .lines()
        .find(|line| line.contains("/.claude/worktrees/"))
        .and_then(|line| line.split(' ').next())
        .map_or_else(|| format!("no implement session: {calls:?}"), str::to_string)
}

#[test]
fn a_run_started_in_a_terminal_gives_the_implement_session_that_terminal() {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let w = world("pty", &agent);

    let (code, drawn) = run_in_pty(&w, &github(), &run_args(&w));

    assert_eq!(code, 0, "{drawn}");
    assert!(drawn.contains("state: review_ready"), "{drawn}");
    assert_eq!(seen(&w), "tty", "{drawn}");
}

#[test]
fn a_run_without_a_terminal_keeps_the_implement_session_headless() {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let w = world("pipe", &agent);
    let provider = github();

    let output = command(&w, &provider, env!("CARGO_BIN_EXE_orq")).args(run_args(&w)).output().unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(seen(&w), "none");
}
