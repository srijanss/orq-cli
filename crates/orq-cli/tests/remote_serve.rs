//! `orq serve` started with and without `--repo`, whatever `remote_control` says, and the removed
//! `orq remote serve`, which only points to it.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOKEN: &str = "serve-token-0123456789abcdef0123456789";

/// A repository with `remote_control` set as given and a good token file, under a fresh root.
fn setup(tag: &str, remote_control: bool) -> (PathBuf, PathBuf) {
    let root = orq_test_support::process_temp_dir().join(format!("orq-serve-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = root.join("repo");
    fs::create_dir_all(repo.join(".orq")).unwrap();
    assert!(Command::new("git").arg("-C").arg(&repo).args(["init", "-q", "-b", "main"]).output().unwrap().status.success());
    fs::write(repo.join(".orq/run.toml"), format!("remote_control = {remote_control}\n")).unwrap();
    let config = root.join("config/orq");
    fs::create_dir_all(&config).unwrap();
    let token = config.join("remote-token");
    fs::write(&token, TOKEN).unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    (root, repo)
}

/// How a server is started: `--repo <repo>`, or from inside the repository with no `--repo`.
#[derive(Clone, Copy)]
enum Repo {
    Flag,
    CurrentDir,
}

fn orq(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    for (name, dir) in [("XDG_STATE_HOME", "state"), ("XDG_DATA_HOME", "data"), ("XDG_CONFIG_HOME", "config"), ("HOME", "home")] {
        command.env(name, root.join(dir));
    }
    command
}

fn command(root: &Path, repo: &Path, how: Repo, extra: &[&str]) -> Command {
    let mut command = orq(root);
    command.arg("serve");
    match how {
        Repo::Flag => command.arg("--repo").arg(repo),
        Repo::CurrentDir => command.current_dir(repo),
    };
    command.args(extra);
    command
}

fn serve(root: &Path, repo: &Path, extra: &[&str]) -> Output {
    command(root, repo, Repo::Flag, extra).output().unwrap()
}

/// A refusal: non-zero exit, one line on stderr, nothing on stdout, no token anywhere.
fn assert_refused(out: &Output, mentions: &str) {
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success(), "must refuse: {stdout}{stderr}");
    assert_eq!(stderr.trim_end().lines().count(), 1, "one line: {stderr:?}");
    assert!(stderr.to_lowercase().contains(mentions), "{stderr:?} should mention {mentions}");
    assert!(!stdout.contains(TOKEN) && !stderr.contains(TOKEN));
}

const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// A running server that is killed and reaped when the test ends, however it ends.
/// Both its output streams are collected as they arrive.
struct Running {
    child: std::process::Child,
    addr: String,
    out: std::sync::Arc<std::sync::Mutex<String>>,
    err: std::sync::Arc<std::sync::Mutex<String>>,
    readers: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn collect(stream: impl std::io::Read + Send + 'static, into: std::sync::Arc<std::sync::Mutex<String>>) -> std::thread::JoinHandle<()> {
    use std::io::BufRead;
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stream).lines().map_while(Result::ok) {
            let mut text = into.lock().unwrap();
            text.push_str(&line);
            text.push('\n');
        }
    })
}

/// Starts the server and waits, bounded, for its listening line.
fn start(root: &Path, repo: &Path, how: Repo) -> Running {
    use std::process::Stdio;
    let mut child = command(root, repo, how, &["--listen", "127.0.0.1:0"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let (stdout, stderr) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    // The guard exists before anything below can fail.
    let mut running = Running { child, addr: String::new(), out: Default::default(), err: Default::default(), readers: Vec::new() };
    running.readers.push(collect(stdout, running.out.clone()));
    running.readers.push(collect(stderr, running.err.clone()));
    let deadline = std::time::Instant::now() + WAIT;
    let first = loop {
        if let Some(line) = running.out.lock().unwrap().lines().next() {
            break line.to_string();
        }
        assert!(std::time::Instant::now() < deadline, "the server did not announce its address in time");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(first.contains("http://127.0.0.1:"), "{first:?}");
    running.addr = first.trim().rsplit("http://").next().unwrap().to_string();
    running
}

impl Running {
    /// Sends `signal`, then waits, bounded, for a clean exit.
    fn stop_with(&mut self, signal: &str) {
        assert!(Command::new("kill").args([signal, &self.child.id().to_string()]).status().unwrap().success());
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "a clean stop after {signal}: {status:?}");
                break;
            }
            assert!(std::time::Instant::now() < deadline, "the server did not stop after {signal}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
    }

    /// Everything the server printed, on either stream, up to now.
    fn output(&self) -> String {
        format!("{}{}", self.out.lock().unwrap(), self.err.lock().unwrap())
    }
}

fn get(addr: &str, token: Option<&str>) -> String {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect_timeout(&addr.parse().unwrap(), WAIT).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.set_write_timeout(Some(WAIT)).unwrap();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    write!(stream, "GET /api/reviews HTTP/1.1\r\nHost: {addr}\r\n{auth}Connection: close\r\n\r\n").unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    reply
}

#[test]
fn a_started_server_answers_401_without_the_token_and_200_with_it_and_stops_on_interrupt() {
    for (tag, how) in [("run-flag", Repo::Flag), ("run-cwd", Repo::CurrentDir)] {
        let (root, repo) = setup(tag, true);
        let mut server = start(&root, &repo, how);
        assert!(get(&server.addr, None).starts_with("HTTP/1.1 401"));
        assert!(get(&server.addr, Some(TOKEN)).starts_with("HTTP/1.1 200"));
        server.stop_with("-INT");
        assert!(!server.output().contains(TOKEN), "the token reached the output");
    }
}

#[test]
fn remote_control_off_does_not_stop_the_server() {
    // `remote_control` is Claude Remote Control for sessions; serving the web never reads it.
    let (root, repo) = setup("off", false);
    let mut server = start(&root, &repo, Repo::Flag);
    assert!(get(&server.addr, Some(TOKEN)).starts_with("HTTP/1.1 200"));
    server.stop_with("-INT");
}

#[test]
fn the_removed_remote_serve_points_to_orq_serve_and_binds_nothing() {
    let (root, repo) = setup("removed", true);
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = free.local_addr().unwrap().to_string();
    drop(free);
    let repo_arg = repo.to_str().unwrap();
    let cases: [&[&str]; 3] = [&["--repo", repo_arg, "--listen", &address], &[], &["--repo", "."]];
    for extra in cases {
        let mut child = orq(&root)
            .args(["remote", "serve"])
            .args(extra)
            .current_dir(&repo)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + WAIT;
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                panic!("`orq remote serve {extra:?}` started instead of pointing to `orq serve`");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{out:?}");
        assert!(out.stdout.is_empty(), "{out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr).trim_end(), "orq: use `orq serve` (same flags)");
        assert!(std::net::TcpListener::bind(&address).is_ok(), "nothing listens on {address}");
    }
}

#[test]
fn a_signal_sent_the_moment_the_address_is_announced_still_stops_it_cleanly() {
    for signal in ["-INT", "-TERM"] {
        for round in 0..10 {
            let (root, repo) = setup(&format!("signal-{round}"), true);
            start(&root, &repo, Repo::Flag).stop_with(signal);
        }
    }
}

#[test]
fn it_refuses_an_address_that_cannot_be_bound_or_is_not_an_address() {
    let (root, repo) = setup("bind", true);
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = taken.local_addr().unwrap().to_string();
    assert_refused(&serve(&root, &repo, &["--listen", &address]), "listen");
    assert_refused(&serve(&root, &repo, &["--listen", "not-an-address"]), "listen");
}
