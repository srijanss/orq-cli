//! `orq serve` restarts itself into the installed orq once no run is active: Settings shows the running
//! and installed builds, `orq serve restart --when-idle` (or the web's button) asks for the restart, it
//! waits for active runs, Cancel stops it, `--now` refuses over active runs unless `--force`, a build under
//! `target/` is never re-executed, and the new server audits both versions.
//!
//! The installed orq is a stand-in script named by `ORQ_INSTALLED_BINARY`: asked for `--version` it prints
//! a version; started any other way it writes its arguments down and waits, as a new server would.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TOKEN: &str = "restart-token-0123456789abcdef01234567";
const WAIT: Duration = Duration::from_secs(15);
const INSTALLED_VERSION: &str = "orq 9.9.9 (fffffff 2030-01-01T00:00:00Z)";

/// A fresh root with a repository and a good token file.
struct World {
    root: PathBuf,
    repo: PathBuf,
}

impl World {
    fn new(tag: &str) -> World {
        let root = orq_test_support::process_temp_dir().join(format!("orq-serve-restart-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        assert!(Command::new("git").arg("-C").arg(&repo).args(["init", "-q", "-b", "main"]).output().unwrap().status.success());
        let config = root.join("config/orq");
        fs::create_dir_all(&config).unwrap();
        let token = config.join("remote-token");
        fs::write(&token, TOKEN).unwrap();
        fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
        World { root, repo }
    }

    /// A stand-in installed orq, outside any `target/` directory.
    fn installed(&self) -> PathBuf {
        let dir = self.root.join("bin");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("orq");
        let record = self.root.join("execd");
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo '{INSTALLED_VERSION}'; exit 0; fi\nprintf '%s\\n' \"$@\" > '{}.tmp' && mv '{}.tmp' '{}'\nexec sleep 30\n",
            record.display(),
            record.display(),
            record.display()
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// The arguments the installed orq was started with, once it has been.
    fn execd(&self) -> Option<Vec<String>> {
        fs::read_to_string(self.root.join("execd")).ok().map(|text| text.lines().map(String::from).collect())
    }

    fn command(&self, installed: &Path) -> Command {
        let mut command = orq_test_support::sandboxed_command(env!("CARGO_BIN_EXE_orq"));
        for (name, dir) in [("XDG_STATE_HOME", "state"), ("XDG_DATA_HOME", "data"), ("XDG_CONFIG_HOME", "config"), ("HOME", "home")] {
            command.env(name, self.root.join(dir));
        }
        command.env("ORQ_INSTALLED_BINARY", installed).env_remove("INVOCATION_ID").env_remove("NOTIFY_SOCKET");
        command
    }

    /// `orq serve restart <args>` as the same user, against this world's servers.
    fn restart(&self, installed: &Path, args: &[&str]) -> Output {
        self.command(installed).arg("serve").arg("restart").args(args).output().unwrap()
    }

    /// Starts `orq serve` and waits, bounded, for its listening line.
    fn serve(&self, installed: &Path) -> Running {
        let mut child = self.command(installed).arg("serve").arg("--repo").arg(&self.repo).args(["--listen", "127.0.0.1:0"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let (stdout, stderr) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
        let running = Running { child, out: Default::default(), err: Default::default() };
        collect(stdout, running.out.clone());
        collect(stderr, running.err.clone());
        running.wait_for(|out| out.contains("http://127.0.0.1:"), "the server did not announce its address");
        running
    }
}

/// A running server that is killed and reaped when the test ends, however it ends.
struct Running {
    child: std::process::Child,
    out: Arc<Mutex<String>>,
    err: Arc<Mutex<String>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn collect(stream: impl std::io::Read + Send + 'static, into: Arc<Mutex<String>>) {
    use std::io::BufRead;
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stream).lines().map_while(Result::ok) {
            let mut text = into.lock().unwrap();
            text.push_str(&line);
            text.push('\n');
        }
    });
}

impl Running {
    fn output(&self) -> String {
        format!("{}{}", self.out.lock().unwrap(), self.err.lock().unwrap())
    }

    fn wait_for(&self, done: impl Fn(&str) -> bool, what: &str) {
        let deadline = Instant::now() + WAIT;
        while !done(&self.output()) {
            assert!(Instant::now() < deadline, "{what}: {}", self.output());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The address of the newest listening line.
    fn addr(&self) -> String {
        let out = self.out.lock().unwrap();
        let line = out.lines().rfind(|line| line.contains("listening on http://")).unwrap().to_string();
        line.trim().rsplit("http://").next().unwrap().to_string()
    }
}

/// `method path` with the bearer token; the status and the JSON body.
fn send(addr: &str, method: &str, path: &str, body: &str) -> (u16, serde_json::Value) {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect_timeout(&addr.parse().unwrap(), WAIT).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    let status = reply.split(' ').nth(1).unwrap().parse().unwrap();
    let json = reply.split_once("\r\n\r\n").map(|(_, body)| serde_json::from_str(body).unwrap_or(serde_json::Value::Null)).unwrap_or_default();
    (status, json)
}

fn version_line() -> String {
    let out = orq_test_support::sandboxed_command(env!("CARGO_BIN_EXE_orq")).arg("--version").output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn the_running_and_installed_builds_appear_in_the_settings() {
    let world = World::new("versions");
    let installed = world.installed();
    let server = world.serve(&installed);
    let (status, body) = send(&server.addr(), "GET", "/api/settings", "");
    assert_eq!(status, 200, "{body}");
    let running = version_line();
    assert!(running.contains(env!("CARGO_PKG_VERSION")) && running.contains('('), "the build carries a commit and time: {running}");
    let server_info = &body["server"];
    assert_eq!(server_info["running"], running.as_str(), "{body}");
    assert_eq!(server_info["installed"], INSTALLED_VERSION, "{body}");
    assert_eq!(server_info["newer_installed"], true, "{body}");
    assert_eq!(server_info["restart_pending"], false, "{body}");
}

impl World {
    fn state(&self) -> PathBuf {
        self.root.join("state/orq")
    }

    /// A run of this repository's project, `stage.started` and still going.
    fn start_run(&self, run_id: &str) {
        use orq_orchestrator::{RunRecord, RunStore};
        RunStore::new(&self.state())
            .start_run(&RunRecord {
                delivery: Default::default(),
                run_id: run_id.into(),
                project_id: "restart-project".into(),
                feature_id: format!("F-{run_id}"),
                remote: "origin".into(),
                provider: "github".into(),
                target_branch: "main".into(),
                base_sha: "a".repeat(40),
                branch: format!("orq/{run_id}"),
                worktree: self.root.join("worktrees").join(run_id),
                repository: String::new(),
                profiles: Default::default(),
            })
            .unwrap();
        self.append(run_id, "stage.started", serde_json::json!({}));
    }

    fn append(&self, run_id: &str, kind: &str, payload: serde_json::Value) {
        let store = orq_orchestrator::RunStore::new(&self.state());
        let n = store.events(run_id).unwrap().len();
        store
            .append_event(&orq_orchestrator::Event {
                schema_version: 1,
                event_id: format!("{run_id}-e{n}"),
                run_id: run_id.into(),
                project_id: "restart-project".into(),
                feature_id: format!("F-{run_id}"),
                kind: kind.into(),
                stage: Some("gate".into()),
                attempt: 1,
                occurred_at: format!("2026-01-01T00:00:{n:02}Z"),
                payload,
            })
            .unwrap();
    }

    fn finish_run(&self, run_id: &str) {
        self.append(run_id, "run.completed", serde_json::json!({"ending": "review_ready"}));
    }

    /// Waits, bounded, for the installed orq to have been started.
    fn wait_for_exec(&self, server: &Running) -> Vec<String> {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(args) = self.execd() {
                return args;
            }
            assert!(Instant::now() < deadline, "the installed orq was never started: {}", server.output());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The arguments `serve` started the server with, as a restart passes them on.
    fn serve_args(&self) -> Vec<String> {
        vec!["serve".into(), "--repo".into(), self.repo.display().to_string(), "--listen".into(), "127.0.0.1:0".into()]
    }

    /// `orq serve` as systemd would start it: `INVOCATION_ID` is set.
    fn serve_under_systemd(&self, installed: &Path) -> Running {
        let mut command = self.command(installed);
        command.env("INVOCATION_ID", "0123abcd");
        let mut child = command.arg("serve").arg("--repo").arg(&self.repo).args(["--listen", "127.0.0.1:0"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let (stdout, stderr) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
        let running = Running { child, out: Default::default(), err: Default::default() };
        collect(stdout, running.out.clone());
        collect(stderr, running.err.clone());
        running.wait_for(|out| out.contains("http://127.0.0.1:"), "the server did not announce its address");
        running
    }
}

/// Waits, bounded, for the server to have taken up a restart it was asked for by signal.
fn wait_for_pending(addr: &str) -> serde_json::Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let (status, body) = send(addr, "GET", "/api/settings", "");
        if status == 200 && body["server"]["restart_pending"] == true {
            return body;
        }
        assert!(Instant::now() < deadline, "the restart never became pending: {body}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn with_no_active_run_a_restart_when_idle_restarts_at_once_into_the_installed_orq_never_a_target_build() {
    let world = World::new("idle");
    let installed = world.installed();
    let server = world.serve(&installed);
    let asked = world.restart(&installed, &["--when-idle"]);
    assert!(asked.status.success(), "{}{}", text(&asked.stdout), text(&asked.stderr));
    assert!(text(&asked.stdout).contains("restart"), "{}", text(&asked.stdout));
    // The test's orq is a build under target/, so the restart starts the installed one, with the same arguments.
    assert_eq!(world.wait_for_exec(&server), world.serve_args());
}

#[test]
fn a_restart_when_idle_waits_for_an_active_run_and_goes_ahead_once_it_ends() {
    let world = World::new("waits");
    let installed = world.installed();
    world.start_run("run-busy");
    let server = world.serve(&installed);
    let asked = world.restart(&installed, &["--when-idle"]);
    assert!(asked.status.success(), "{}", text(&asked.stderr));
    assert!(text(&asked.stdout).contains("waiting for 1 run"), "{}", text(&asked.stdout));
    let body = wait_for_pending(&server.addr());
    assert_eq!(body["server"]["waiting_for"], 1, "{body}");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(world.execd().is_none(), "restarted over an active run");
    world.finish_run("run-busy");
    assert_eq!(world.wait_for_exec(&server), world.serve_args());
}

#[test]
fn cancel_stops_a_pending_restart() {
    let world = World::new("cancel");
    let installed = world.installed();
    world.start_run("run-busy");
    let server = world.serve(&installed);
    assert!(world.restart(&installed, &["--when-idle"]).status.success());
    wait_for_pending(&server.addr());
    let (status, body) = send(&server.addr(), "POST", "/api/server/restart/cancel", "");
    assert_eq!((status, body["cancelled"].clone()), (200, serde_json::json!(true)), "{body}");
    world.finish_run("run-busy");
    std::thread::sleep(Duration::from_millis(2000));
    assert!(world.execd().is_none(), "a cancelled restart went ahead: {}", server.output());
    let (status, body) = send(&server.addr(), "GET", "/api/settings", "");
    assert_eq!((status, body["server"]["restart_pending"].clone()), (200, serde_json::json!(false)), "{body}");
}

#[test]
fn now_is_refused_over_active_runs_and_names_them_unless_forced() {
    let world = World::new("now");
    let installed = world.installed();
    world.start_run("run-busy");
    let server = world.serve(&installed);
    let refused = world.restart(&installed, &["--now"]);
    assert!(!refused.status.success());
    assert!(text(&refused.stderr).contains("run-busy"), "{}", text(&refused.stderr));
    std::thread::sleep(Duration::from_millis(500));
    assert!(world.execd().is_none());
    let forced = world.restart(&installed, &["--now", "--force"]);
    assert!(forced.status.success(), "{}", text(&forced.stderr));
    assert_eq!(world.wait_for_exec(&server), world.serve_args());
}

#[test]
fn a_restart_is_refused_with_the_reason_when_there_is_no_installed_orq_and_serve_runs_from_target() {
    let world = World::new("no-installed");
    let missing = world.root.join("bin/missing-orq");
    let _server = world.serve(&missing);
    let refused = world.restart(&missing, &["--when-idle"]);
    assert!(!refused.status.success());
    assert!(text(&refused.stderr).contains("target/") && text(&refused.stderr).contains("missing-orq"), "{}", text(&refused.stderr));
}

#[test]
fn a_restart_with_no_server_running_says_so() {
    let world = World::new("none");
    let installed = world.installed();
    let out = world.restart(&installed, &["--when-idle"]);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("no orq serve is running"), "{}", text(&out.stderr));
}

#[test]
fn under_systemd_a_restart_exits_with_a_status_systemd_restarts() {
    let world = World::new("systemd");
    let installed = world.installed();
    let mut server = world.serve_under_systemd(&installed);
    assert!(world.restart(&installed, &["--when-idle"]).status.success());
    let deadline = Instant::now() + WAIT;
    let status = loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "serve did not exit: {}", server.output());
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(75), "{}", server.output());
    assert!(world.execd().is_none(), "systemd starts the new server, not serve");
}

#[test]
fn the_new_server_audits_the_restart_with_both_versions() {
    let world = World::new("audit");
    // The installed orq is this build, hard-linked outside target/, so the restart starts a real server.
    let dir = world.root.join("bin");
    fs::create_dir_all(&dir).unwrap();
    let installed = dir.join("orq");
    fs::hard_link(env!("CARGO_BIN_EXE_orq"), &installed).or_else(|_| fs::copy(env!("CARGO_BIN_EXE_orq"), &installed).map(|_| ())).unwrap();
    let server = world.serve(&installed);
    let first = server.addr();
    assert!(world.restart(&installed, &["--when-idle"]).status.success());
    server.wait_for(|out| out.matches("listening on http://").count() == 2, "the new server did not start");
    assert_ne!(server.addr(), first, "a new server listens");
    let running = version_line();
    let log = fs::read_to_string(world.state().join("remote-actions.jsonl")).unwrap();
    let line: serde_json::Value = log.lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok()).find(|line| line["action"] == "server.restarted").unwrap_or_else(|| panic!("{log}"));
    let reason = line["reason"].as_str().unwrap();
    assert!(reason.contains(&format!("from {running}")) && reason.contains(&format!("to {running}")), "{reason}");
    let (status, _) = send(&server.addr(), "GET", "/api/settings", "");
    assert_eq!(status, 200);
}

#[test]
fn the_token_alone_cannot_restart_the_server_from_the_web() {
    let world = World::new("web-token");
    let installed = world.installed();
    fs::write(world.root.join("config/orq/config.toml"), "remote_actions = true\n").unwrap();
    let server = world.serve(&installed);
    let (status, body) = send(&server.addr(), "POST", "/api/server/restart", "");
    assert_eq!(status, 403, "{body}");
    assert!(body["error"].as_str().unwrap_or_default().contains("passkey"), "{body}");
    assert!(world.execd().is_none());
}

#[test]
fn the_served_web_app_offers_the_restart_and_its_pending_state() {
    use std::io::{Read, Write};
    let world = World::new("web-app");
    let installed = world.installed();
    let server = world.serve(&installed);
    let addr = server.addr();
    let mut stream = std::net::TcpStream::connect_timeout(&addr.parse().unwrap(), WAIT).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    write!(stream, "GET /app.react.js HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let app = String::from_utf8_lossy(&reply);
    for wanted in ["/api/server/restart", "A newer orq is installed", "Restart pending: waiting for", "orq will restart after this run; start anyway?"] {
        assert!(app.contains(wanted), "the served app has no {wanted:?}");
    }
}
