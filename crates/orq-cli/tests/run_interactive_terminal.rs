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
/// stdin and its stdout were each a terminal to `seen` before it runs `agent`.
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
            "#!/bin/sh\ni=-; o=-; [ -t 0 ] && i=t; [ -t 1 ] && o=t\necho \"$i$o $PWD\" >> '{seen}'\nprintf '%s\\n' \"$*\" >> '{seen}.args'\ncp .claude/settings.local.json '{seen}.settings' 2>/dev/null\nexec '{agent}' \"$@\"\n",
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
    run_in_pty_with(w, provider, Streams::Terminal, args)
}

/// Which of orq's standard streams are the terminal; the others are moved off it.
#[derive(Clone, Copy)]
enum Streams {
    Terminal,
    /// Input is `/dev/null`; output and errors are the terminal.
    InputElsewhere,
    /// Output goes to a file; input and errors are the terminal.
    OutputElsewhere,
}

/// As `run_in_pty`, with `streams` deciding which streams are the terminal. Output moved off the
/// terminal is appended to what is returned.
fn run_in_pty_with(w: &World, provider: &FakeProvider, streams: Streams, args: &[String]) -> (i32, String) {
    drive(w, provider, streams, 60, env!("CARGO_BIN_EXE_orq"), args)
}

/// Runs `program <args>` on a fresh pseudo-terminal, reading it until no process holds it any
/// more or `deadline_secs` pass; past the deadline everything in the session is killed and the
/// exit code is 124.
fn drive(
    w: &World,
    provider: &FakeProvider,
    streams: Streams,
    deadline_secs: u32,
    program: &str,
    args: &[String],
) -> (i32, String) {
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
    if sys.argv[1] == "input-elsewhere":
        os.dup2(os.open("/dev/null", os.O_RDONLY), 0)
    elif sys.argv[1] == "output-elsewhere":
        os.dup2(os.open(sys.argv[2], os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600), 1)
    os.close(fd)
    os.close(slave)
    os.execv(sys.argv[3], sys.argv[3:])
os.close(slave)
out = bytearray()
status = None
drained = False
deadline = time.time() + float(os.environ.get("PTY_DEADLINE", "60"))
# Read until the terminal reports end of stream (every holder of it has gone), not merely until
# orq exits: output written just before the exit may still be buffered.
while time.time() < deadline:
    r, _, _ = select.select([fd], [], [], 0.1)
    if r:
        try:
            chunk = os.read(fd, 4096)
        except OSError:
            chunk = b""
        if not chunk:
            drained = True
            break
        out.extend(chunk)
    if status is None:
        done, st = os.waitpid(pid, os.WNOHANG)
        if done:
            status = st
end = time.time() + 5
while status is None and time.time() < end:
    done, st = os.waitpid(pid, os.WNOHANG)
    if done:
        status = st
    else:
        time.sleep(0.05)
if status is None or not drained:
    # orq, or something it left holding the terminal, outlived the deadline: end everything in
    # the session orq leads, whatever process group it moved to, and fail rather than report a
    # partial capture.
    for _ in range(3):
        for entry in os.listdir("/proc"):
            if not entry.isdigit():
                continue
            try:
                with open("/proc/" + entry + "/stat") as stat:
                    fields = stat.read().rsplit(")", 1)[1].split()
            except OSError:
                continue
            # After the command name: state, ppid, pgrp, session.
            if int(fields[3]) == pid:
                try:
                    os.kill(int(entry), signal.SIGKILL)
                except OSError:
                    pass
    if status is None:
        os.waitpid(pid, 0)
    what = b"orq did not exit" if status is None else b"the terminal was still held open after orq exited"
    sys.stdout.buffer.write(bytes(out) + b"\n[driver: " + what + b"; killed]")
    sys.exit(124)
sys.stdout.buffer.write(bytes(out))
sys.exit(os.waitstatus_to_exitcode(status))
"#;
    let mode = match streams {
        Streams::Terminal => "terminal",
        Streams::InputElsewhere => "input-elsewhere",
        Streams::OutputElsewhere => "output-elsewhere",
    };
    let elsewhere = w.seen.with_file_name("stdout");
    let output = command(w, provider, "python3")
        .env("PTY_DEADLINE", deadline_secs.to_string())
        .args(["-I", "-c", DRIVER, mode])
        .arg(&elsewhere)
        .arg(program)
        .args(args)
        .output()
        .unwrap();
    let mut drawn = String::from_utf8_lossy(&output.stdout).into_owned();
    drawn.push_str(&std::fs::read_to_string(&elsewhere).unwrap_or_default());
    (output.status.code().unwrap_or(-1), drawn)
}

/// Whether the implement session, the first `claude` started in the run's worktree, had a
/// terminal: "tty" when both its input and output were the terminal, "none" when neither was, and
/// otherwise which one was. Later headless calls (the review draft, the push notification) are
/// left out.
fn seen(w: &World) -> String {
    let calls = std::fs::read_to_string(&w.seen).unwrap_or_default();
    match calls.lines().find(|line| line.contains("/.claude/worktrees/")).and_then(|line| line.split(' ').next()) {
        Some("tt") => "tty".to_string(),
        Some("--") => "none".to_string(),
        Some("t-") => "only its input was the terminal".to_string(),
        Some("-t") => "only its output was the terminal".to_string(),
        _ => format!("no implement session: {calls:?}"),
    }
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

#[test]
fn only_a_session_with_a_terminal_is_told_someone_is_watching_and_can_answer() {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let prompt_of = |w: &World| std::fs::read_to_string(format!("{}.args", w.seen.display())).unwrap_or_default();

    let attended = world("attended-prompt", &agent);
    let (code, drawn) = run_in_pty(&attended, &github(), &run_args(&attended));
    assert_eq!(code, 0, "{drawn}");
    let prompt = prompt_of(&attended);
    assert!(prompt.contains("AskUserQuestion"), "{prompt}");
    assert!(prompt.contains("watching this session"), "{prompt}");

    let headless = world("headless-prompt", &agent);
    let output = command(&headless, &github(), env!("CARGO_BIN_EXE_orq")).args(run_args(&headless)).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stderr));
    let prompt = prompt_of(&headless);
    assert!(prompt.contains("COM-01"), "{prompt}");
    assert!(!prompt.contains("AskUserQuestion"), "{prompt}");
    assert!(!prompt.contains("watching this session"), "{prompt}");
}

#[test]
fn only_a_session_with_a_terminal_gets_the_hook_that_says_it_needs_its_person() {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let settings_of = |w: &World| std::fs::read_to_string(format!("{}.settings", w.seen.display())).unwrap_or_default();

    let attended = world("attended-hook", &agent);
    let (code, drawn) = run_in_pty(&attended, &github(), &run_args(&attended));
    assert_eq!(code, 0, "{drawn}");
    let settings: serde_json::Value = serde_json::from_str(&settings_of(&attended)).unwrap();
    let hook = settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str().unwrap();
    assert_eq!(settings["hooks"]["PreToolUse"][0]["matcher"], "AskUserQuestion");
    assert_eq!(settings["hooks"]["Notification"][0]["matcher"], "permission_prompt");
    assert!(hook.contains(" run needs-input "), "{hook}");
    assert!(hook.contains(&format!("'{}'", attended.repo.root().display())), "{hook}");

    let headless = world("headless-hook", &agent);
    let output = command(&headless, &github(), env!("CARGO_BIN_EXE_orq")).args(run_args(&headless)).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(!settings_of(&headless).contains("needs-input"));
}

#[test]
fn a_headless_run_resuming_on_an_attended_worktree_takes_the_hook_out() {
    let agent = FakeAgent::builder().exit_code(3).build();
    let settings_of = |w: &World| std::fs::read_to_string(format!("{}.settings", w.seen.display())).unwrap_or_default();
    let w = world("reused-hook", &agent);

    let (_, drawn) = run_in_pty(&w, &github(), &run_args(&w));
    assert!(settings_of(&w).contains("needs-input"), "{drawn}");

    let mut args = run_args(&w);
    args.push("--resume".to_string());
    let output = command(&w, &github(), env!("CARGO_BIN_EXE_orq")).args(args).output().unwrap();

    let sessions = std::fs::read_to_string(&w.seen).unwrap();
    let places: Vec<&str> = sessions.lines().map(|line| line.split_once(' ').unwrap().1).collect();
    assert!(places.len() >= 2, "{sessions}\n{}", String::from_utf8_lossy(&output.stderr));
    assert!(places.iter().all(|place| *place == places[0]), "the resumed run works in the same worktree: {sessions}");
    assert!(!settings_of(&w).contains("needs-input"), "{}", settings_of(&w));
}

#[test]
fn a_run_whose_input_is_not_the_terminal_keeps_the_implement_session_headless() {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let w = world("no-input", &agent);

    let (code, drawn) = run_in_pty_with(&w, &github(), Streams::InputElsewhere, &run_args(&w));

    assert_eq!(code, 0, "{drawn}");
    assert_eq!(seen(&w), "none", "{drawn}");
}

#[test]
fn a_run_whose_output_is_not_the_terminal_keeps_the_implement_session_headless() {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let w = world("no-output", &agent);

    let (code, drawn) = run_in_pty_with(&w, &github(), Streams::OutputElsewhere, &run_args(&w));

    assert_eq!(code, 0, "{drawn}");
    assert!(drawn.contains("state: review_ready"), "{drawn}");
    assert_eq!(seen(&w), "none", "{drawn}");
}

#[test]
fn an_interactive_session_that_fails_fails_the_run_and_orq_still_reports_on_the_terminal() {
    let agent = FakeAgent::builder().stderr("the agent gave up\n").exit_code(3).build();
    let w = world("pty-fails", &agent);

    let (code, drawn) = run_in_pty(&w, &github(), &run_args(&w));

    assert_eq!(seen(&w), "tty", "{drawn}");
    // The session wrote to the terminal itself rather than to the stage log.
    assert!(drawn.contains("the agent gave up"), "{drawn}");
    assert_eq!(code, 31, "{drawn}");
    assert!(drawn.contains("state: failed"), "{drawn}");
}

/// The process ids printed as `held <pid>` lines.
fn held_pids(drawn: &str) -> Vec<u32> {
    drawn.lines().filter_map(|line| line.trim().strip_prefix("held ")?.trim().parse().ok()).collect()
}

/// Whether `pid` has ended (gone, or a zombie waiting to be reaped) within two seconds.
fn ended(pid: u32) -> bool {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| stat.rsplit(')').next().and_then(|rest| rest.split_whitespace().next()).map(str::to_string));
        if matches!(state.as_deref(), None | Some("Z")) {
            return true;
        }
        if std::time::Instant::now() > until {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn the_terminal_driver_fails_when_something_still_holds_the_terminal_at_its_deadline() {
    let agent = FakeAgent::builder().build();
    let w = world("held", &agent);
    // One holder in orq's own process group and one, through job control, in a group of its own.
    let script = "trap '' HUP; sleep 30 & echo held $!; set -m; sleep 30 & echo held $!";
    let args = ["-c".to_string(), script.to_string()];

    let (code, drawn) = drive(&w, &github(), Streams::Terminal, 2, "/bin/sh", &args);

    assert!(drawn.contains("the terminal was still held open after orq exited"), "{drawn}");
    assert_eq!(code, 124, "{drawn}");
    let held = held_pids(&drawn);
    assert_eq!(held.len(), 2, "{drawn}");
    for pid in held {
        assert!(ended(pid), "holder {pid} survived the driver: {drawn}");
    }
}

#[test]
fn the_terminal_driver_fails_and_ends_orq_when_orq_has_not_exited_by_its_deadline() {
    let agent = FakeAgent::builder().build();
    let w = world("stuck", &agent);
    let args = ["-c".to_string(), "echo held $$; exec sleep 30".to_string()];

    let (code, drawn) = drive(&w, &github(), Streams::Terminal, 2, "/bin/sh", &args);

    assert!(drawn.contains("orq did not exit; killed"), "{drawn}");
    assert_eq!(code, 124, "{drawn}");
    let held = held_pids(&drawn);
    assert_eq!(held.len(), 1, "{drawn}");
    assert!(ended(held[0]), "orq {} survived the driver: {drawn}", held[0]);
}

#[test]
fn a_red_gate_opens_its_fix_session_on_the_terminal_and_closes_it_when_the_session_says_it_is_done() {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let w = world("gate-fix", &agent);
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), "gate = [\"sh\", \"-c\", \"test -f fixed\"]\n").unwrap();
    // The fix session records how it was started, works long enough for a line to be drawn over it if
    // anything were to draw one, commits, leaves the marker it was told about, and then just stays open.
    let (fix_seen, fix_args, late) = (w.seen.with_file_name("fix-seen"), w.seen.with_file_name("fix-args"), w.seen.with_file_name("late"));
    std::fs::rename(w.bin.join("claude"), w.bin.join("claude-inner")).unwrap();
    std::fs::write(
        w.bin.join("claude"),
        format!(
            "#!/bin/sh\ncase \"$*\" in *'local gate failed'*)\n[ -t 0 ] && [ -t 1 ] && echo tty > '{seen}'\nprintf '%s\\n' \"$*\" > '{args}'\nsleep 2\ntouch fixed; git add fixed; git -c user.name=t -c user.email=t@example.invalid commit -q -m 'fix: repair the gate'\nmarker=$(printf '%s' \"$*\" | sed -n 's/.*create the empty file \\([^ ]*\\) and then stop.*/\\1/p')\ntouch \"$marker\"\nsleep 20\n: > '{late}'\nexit 0;;\nesac\nexec '{inner}' \"$@\"\n",
            seen = fix_seen.display(),
            args = fix_args.display(),
            late = late.display(),
            inner = w.bin.join("claude-inner").display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(w.bin.join("claude"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let (code, drawn) = run_in_pty(&w, &github(), &run_args(&w));

    assert_eq!(code, 0, "{drawn}");
    assert!(drawn.contains("state: review_ready"), "{drawn}");
    assert_eq!(std::fs::read_to_string(&fix_seen).unwrap_or_default().trim(), "tty", "{drawn}");
    let args = std::fs::read_to_string(&fix_args).unwrap();
    assert!(!format!(" {args} ").contains(" -p "), "an attended fix session is not headless: {args}");
    assert!(args.contains("--add-dir") && args.contains("/logs/"), "the session may read the logs: {args}");
    assert!(!late.exists(), "the session was closed by its marker");
    assert!(!drawn.contains("gate_fix"), "nothing is drawn over the session: {drawn}");
}
