//! `orq remote serve`: the refusals, and a started server.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOKEN: &str = "serve-token-0123456789abcdef0123456789";

/// A repository with remote control on and a good token file, under a fresh root.
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

fn command(root: &Path, repo: &Path, extra: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    command.args(["remote", "serve", "--repo"]).arg(repo).args(extra);
    for (name, dir) in [("XDG_STATE_HOME", "state"), ("XDG_DATA_HOME", "data"), ("XDG_CONFIG_HOME", "config"), ("HOME", "home")] {
        command.env(name, root.join(dir));
    }
    command
}

fn serve(root: &Path, repo: &Path, extra: &[&str]) -> Output {
    command(root, repo, extra).output().unwrap()
}

/// A refusal: non-zero exit, one line on stderr, nothing on stdout, no token anywhere.
fn assert_refused(out: &Output, mentions: &str) {
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success(), "must refuse: {stdout}{stderr}");
    assert_eq!(stderr.trim_end().lines().count(), 1, "one line: {stderr:?}");
    assert!(stderr.to_lowercase().contains(mentions), "{stderr:?} should mention {mentions}");
    assert!(!stdout.contains(TOKEN) && !stderr.contains(TOKEN));
}

#[test]
fn it_refuses_to_start_when_remote_control_is_off() {
    let (root, repo) = setup("off", false);
    assert_refused(&serve(&root, &repo, &["--listen", "127.0.0.1:0"]), "remote control");
}

#[test]
fn it_refuses_a_token_file_that_is_missing_too_short_or_too_open() {
    let (root, repo) = setup("token", true);
    let token = root.join("config/orq/remote-token");
    let listen = ["--listen", "127.0.0.1:0"];

    fs::set_permissions(&token, fs::Permissions::from_mode(0o644)).unwrap();
    assert_refused(&serve(&root, &repo, &listen), "token");

    fs::write(&token, "short").unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    assert_refused(&serve(&root, &repo, &listen), "token");

    fs::remove_file(&token).unwrap();
    assert_refused(&serve(&root, &repo, &listen), "token");
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
fn start(root: &Path, repo: &Path) -> Running {
    use std::process::Stdio;
    let mut child = command(root, repo, &["--listen", "127.0.0.1:0"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
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
    let (root, repo) = setup("run", true);
    let mut server = start(&root, &repo);
    assert!(get(&server.addr, None).starts_with("HTTP/1.1 401"));
    assert!(get(&server.addr, Some(TOKEN)).starts_with("HTTP/1.1 200"));
    server.stop_with("-INT");
    assert!(!server.output().contains(TOKEN), "the token reached the output");
}

#[test]
fn a_signal_sent_the_moment_the_address_is_announced_still_stops_it_cleanly() {
    for signal in ["-INT", "-TERM"] {
        for round in 0..10 {
            let (root, repo) = setup(&format!("signal-{round}"), true);
            start(&root, &repo).stop_with(signal);
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
