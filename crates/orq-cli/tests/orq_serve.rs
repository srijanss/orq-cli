//! A plain `orq serve`: no setup beyond being inside a repository.

use std::fs;
use std::io::{BufRead, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// A repository with no settings file and no token, under a fresh root.
fn bare_repo(tag: &str) -> (PathBuf, PathBuf) {
    let root = orq_test_support::process_temp_dir().join(format!("orq-plain-serve-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert!(Command::new("git").arg("-C").arg(&repo).args(["init", "-q", "-b", "main"]).output().unwrap().status.success());
    (root, repo)
}

struct Running {
    child: Child,
    addr: String,
    printed: String,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(root: &Path, repo: &Path) -> Running {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    command.args(["serve", "--listen", "127.0.0.1:0"]).current_dir(repo).stdout(Stdio::piped()).stderr(Stdio::null());
    for (name, dir) in [("XDG_STATE_HOME", "state"), ("XDG_DATA_HOME", "data"), ("XDG_CONFIG_HOME", "config"), ("HOME", "home")] {
        command.env(name, root.join(dir));
    }
    let mut child = command.spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut running = Running { child, addr: String::new(), printed: String::new() };
    // Read on a worker so a stalled start cannot hang the test: every wait is bounded, and on a
    // timeout the guard above kills and reaps the child.
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    // The address line comes first; the guidance on reading the token follows it.
    let first = receiver.recv_timeout(WAIT).expect("the server did not announce its address in time");
    running.addr = first.trim().rsplit("http://").next().unwrap().to_string();
    running.printed = first;
    for _ in 0..2 {
        let line = receiver.recv_timeout(WAIT).unwrap_or_else(|_| panic!("the server stopped printing its guidance: {}", running.printed));
        running.printed.push('\n');
        running.printed.push_str(&line);
    }
    running
}

fn get(addr: &str, path: &str, token: Option<&str>) -> String {
    let mut stream = std::net::TcpStream::connect_timeout(&addr.parse().unwrap(), WAIT).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: {addr}\r\n{auth}Connection: close\r\n\r\n").unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    reply
}

fn token_file(root: &Path) -> PathBuf {
    root.join("config/orq/remote-token")
}

#[test]
fn a_plain_serve_creates_a_private_token_serves_the_page_and_guards_the_api() {
    let (root, repo) = bare_repo("first");
    let server = start(&root, &repo);
    let path = token_file(&root);
    let token = fs::read_to_string(&path).expect("the token is created on first use").trim().to_string();
    assert!(token.len() >= 32, "{token:?}");
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert!(server.printed.contains(&path.display().to_string()), "it says where the token is: {}", server.printed);
    assert!(!server.printed.contains(&token), "the token itself is never printed");
    assert!(get(&server.addr, "/", None).starts_with("HTTP/1.1 200"));
    assert!(get(&server.addr, "/api/reviews", None).starts_with("HTTP/1.1 401"));
    assert!(get(&server.addr, "/api/reviews", Some(&token)).starts_with("HTTP/1.1 200"));
}

#[test]
fn an_existing_token_is_kept() {
    let (root, repo) = bare_repo("kept");
    drop(start(&root, &repo));
    let first = fs::read_to_string(token_file(&root)).unwrap();
    drop(start(&root, &repo));
    assert_eq!(fs::read_to_string(token_file(&root)).unwrap(), first);
}

#[test]
fn the_command_that_reads_the_token_is_safe_to_paste_whatever_the_path_holds() {
    let (root, repo) = bare_repo("q uote$(echo hi)'s");
    let server = start(&root, &repo);
    let path = token_file(&root).display().to_string();
    let quoted = format!("'{}'", path.replace('\'', "'\\''"));
    assert!(server.printed.lines().any(|line| line == format!("read it with: cat -- {quoted}")), "{}", server.printed);
}

/// Runs a plain serve that must refuse: non-zero exit, no listening line.
fn refused(root: &Path, repo: &Path) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    command.args(["serve", "--listen", "127.0.0.1:0"]).current_dir(repo);
    command.envs([("XDG_STATE_HOME", root.join("state")), ("XDG_DATA_HOME", root.join("data")), ("XDG_CONFIG_HOME", root.join("config")), ("HOME", root.join("home"))]);
    let out = command.output().unwrap();
    assert!(!out.status.success(), "must refuse: {}", String::from_utf8_lossy(&out.stdout));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("listening"), "it must not announce an address");
    out
}

#[test]
fn an_existing_token_that_is_too_open_too_short_empty_or_a_link_is_refused_and_left_alone() {
    use std::os::unix::fs::symlink;
    let secret = "0123456789abcdef0123456789abcdef-existing";
    for case in ["open", "short", "empty", "link"] {
        let (root, repo) = bare_repo(&format!("bad-{case}"));
        fs::create_dir_all(root.join("config/orq")).unwrap();
        let path = token_file(&root);
        let target = root.join("elsewhere");
        let (contents, mode) = match case {
            "open" => (secret, 0o644),
            "short" => ("short", 0o600),
            _ => ("", 0o600),
        };
        if case == "link" {
            fs::write(&target, secret).unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
            symlink(&target, &path).unwrap();
        } else {
            fs::write(&path, contents).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        }
        let before = fs::read(&path).unwrap();
        let out = refused(&root, &repo);
        assert!(String::from_utf8_lossy(&out.stderr).to_lowercase().contains("token"), "{case}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(fs::read(&path).unwrap(), before, "{case}: the file is not replaced or rewritten");
        if case == "link" {
            assert!(fs::symlink_metadata(&path).unwrap().file_type().is_symlink(), "the link is kept");
            assert_eq!(fs::read_to_string(&target).unwrap(), secret);
        }
    }
}
