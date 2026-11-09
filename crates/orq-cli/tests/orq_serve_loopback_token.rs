//! `orq serve --passkey-host <name> --no-loopback-token`: with passkey sign-in on, the token no
//! longer opens the API even on a direct local connection.

use std::fs;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const WAIT: std::time::Duration = std::time::Duration::from_secs(10);
const HOST: &str = "box.tail1234.ts.net";

struct Running {
    child: Child,
    addr: String,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn repo(tag: &str) -> (PathBuf, PathBuf) {
    let root = orq_test_support::process_temp_dir().join(format!("orq-loopback-token-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert!(Command::new("git").arg("-C").arg(&repo).args(["init", "-q", "-b", "main"]).output().unwrap().status.success());
    (root, repo)
}

fn command(root: &Path, repo: &Path, extra: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    command.args(["serve", "--listen", "127.0.0.1:0"]).args(extra).current_dir(repo);
    for (name, dir) in [("XDG_STATE_HOME", "state"), ("XDG_DATA_HOME", "data"), ("XDG_CONFIG_HOME", "config"), ("HOME", "home")] {
        command.env(name, root.join(dir));
    }
    command
}

fn start(root: &Path, repo: &Path, extra: &[&str]) -> Running {
    let mut child = command(root, repo, extra).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut running = Running { child, addr: String::new() };
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let first = receiver.recv_timeout(WAIT).expect("the server did not announce its address in time");
    running.addr = first.trim().rsplit("http://").next().unwrap().to_string();
    // The guidance lines follow the address; the token exists before the address is printed.
    for _ in 0..3 {
        let _ = receiver.recv_timeout(WAIT);
    }
    running
}

fn api_status(addr: &str, token: &str) -> String {
    let mut stream = std::net::TcpStream::connect_timeout(&addr.parse().unwrap(), WAIT).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    write!(stream, "GET /api/reviews HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n").unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    reply.lines().next().unwrap_or("").to_string()
}

fn token(root: &Path) -> String {
    fs::read_to_string(root.join("config/orq/remote-token")).unwrap().trim().to_string()
}

#[test]
fn the_token_keeps_working_on_a_direct_local_connection_unless_the_flag_switches_it_off() {
    let (root, repo) = repo("flag");
    let on = start(&root, &repo, &["--passkey-host", HOST]);
    assert!(api_status(&on.addr, &token(&root)).contains("200"));
    drop(on);
    let off = start(&root, &repo, &["--passkey-host", HOST, "--no-loopback-token"]);
    assert!(api_status(&off.addr, &token(&root)).contains("401"));
}

#[test]
fn the_flag_means_nothing_without_a_passkey_host_and_is_refused() {
    let (root, repo) = repo("alone");
    let out = command(&root, &repo, &["--no-loopback-token"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--passkey-host"), "{}", String::from_utf8_lossy(&out.stderr));
}
