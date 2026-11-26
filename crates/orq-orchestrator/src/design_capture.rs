//! A frontend run's design capture: a deterministic step, no agent. It starts the project's page with
//! `[design].start_command` on a port of the run's own, waits until `ready_path` answers 2xx, then
//! screenshots and measures every route at every viewport in headless Chromium and stops both.
//!
//! The browser is driven over the DevTools protocol on a pipe (`--remote-debugging-pipe`), so nothing
//! listens on a port of its own. The page is data: what the capture does (its routes, viewports, files
//! and commands) is decided before the page loads, and its measuring script runs in an isolated world
//! the page's own scripts cannot reach.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use crate::child_groups::TrackedChild;
use crate::design_package::DesignManifest;
use crate::design_tokens::token_rule_selectors;
use crate::run_settings::DesignSettings;

/// Under the run's logs: one folder per round.
const RUN_DESIGN_DIR: &str = "design";
const CAPTURE_FILE: &str = "capture.json";
const MEASUREMENTS_FILE: &str = "measurements.json";

/// How long one page may take to load, and the browser to answer one call.
const PAGE_LOAD: Duration = Duration::from_secs(30);
const CALL: Duration = Duration::from_secs(30);
/// How long a just-started Chromium has to give its first answer before it is taken as stuck.
const FIRST_ANSWER: Duration = Duration::from_secs(10);
/// How long the capture waits for a page's fonts and images.
const ASSETS_BOUND_MS: u64 = 5_000;
/// The tallest full-page capture, in CSS pixels; a longer page is cut there.
const MAX_PAGE_HEIGHT: u64 = 16_384;
const MAX_PAGE_WIDTH: u64 = 8_192;
/// The largest message the browser may send (a screenshot, base64).
const MAX_MESSAGE: usize = 256 * 1024 * 1024;
/// The most routes a package's `DESIGN.md` may name.
const MAX_ROUTES: usize = 20;
const MAX_ROUTE_LEN: usize = 200;

/// One viewport to capture at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureViewport {
    /// The reference name: letters, digits, `-` and `_`.
    pub name: String,
    pub width: u32,
    pub height: u32,
}

/// One capture round of a frontend run.
#[derive(Debug, Clone, Copy)]
pub struct CaptureRun<'a> {
    /// The run's worktree, where the start command runs.
    pub worktree: &'a Path,
    pub settings: &'a DesignSettings,
    /// The feature's routes (see [`design_routes`]).
    pub routes: &'a [String],
    pub viewports: &'a [CaptureViewport],
    /// The run's log folder; the round goes in its `design/round-<n>/`.
    pub run_logs: &'a Path,
    pub round: u32,
    /// The Chromium to use; `None` finds the one the web tests use (see [`find_chromium_in`]).
    pub browser: Option<&'a Path>,
}

/// How a capture round ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// The page was captured; a route that failed is in the record's `failures`.
    Captured(CaptureRecord),
    /// The capture could not take place (the page never got ready, no browser): never a pass.
    Blocked(String),
}

/// `capture.json` of a captured round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaptureRecord {
    pub round: u32,
    /// The worktree's `HEAD` when the page was captured.
    pub commit: Option<String>,
    pub browser_version: String,
    pub port: u16,
    pub pages: Vec<PageCapture>,
    /// One line per page that failed.
    pub failures: Vec<String>,
}

/// One route at one viewport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PageCapture {
    pub route: String,
    pub viewport: String,
    pub viewport_width: u32,
    pub viewport_height: u32,
    /// The PNG, relative to the round's folder: `<viewport>/<route-slug>.png`.
    pub image: Option<String>,
    pub image_width: Option<u32>,
    pub image_height: Option<u32>,
    pub failure: Option<String>,
}

/// The routes a package's `DESIGN.md` lists under a `Routes` heading, one per list item, each a plain
/// path; `/` when it lists none. Only the paths are read: the rest of the text is never acted on.
pub fn design_routes(design_doc: Option<&str>) -> Vec<String> {
    let mut routes: Vec<String> = Vec::new();
    let mut in_routes = false;
    for line in design_doc.unwrap_or("").lines() {
        let line = line.trim();
        if line.starts_with('#') {
            in_routes = line
                .trim_start_matches('#')
                .trim()
                .eq_ignore_ascii_case("routes");
            continue;
        }
        if !in_routes {
            continue;
        }
        let Some(item) = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| line.strip_prefix("+ "))
        else {
            continue;
        };
        let word = item
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches('`');
        if plain_route(word) && !routes.iter().any(|r| r == word) && routes.len() < MAX_ROUTES {
            routes.push(word.to_string());
        }
    }
    if routes.is_empty() {
        routes.push("/".to_string());
    }
    routes
}

/// The viewports a round captures: each one a frozen reference was assigned to, and each in
/// `[design.viewports]` (built-ins included), by name. A reference's size wins over the settings'.
pub fn capture_viewports(
    manifest: Option<&DesignManifest>,
    settings: &DesignSettings,
) -> Vec<CaptureViewport> {
    let mut viewports: BTreeMap<String, (u32, u32)> = settings
        .viewports
        .iter()
        .map(|(name, size)| (name.clone(), (size.width, size.height)))
        .collect();
    for file in manifest.into_iter().flat_map(|m| &m.files) {
        if let (true, Some(viewport)) = (file.kind == "reference", &file.viewport) {
            viewports.insert(viewport.name.clone(), (viewport.width, viewport.height));
        }
    }
    viewports
        .into_iter()
        .map(|(name, (width, height))| CaptureViewport {
            name,
            width,
            height,
        })
        .collect()
}

/// A path on the dev server: `/`, then printable ASCII with no `..` segment, `\`, `#` or `//` start.
fn plain_route(route: &str) -> bool {
    route.starts_with('/')
        && !route.starts_with("//")
        && route.len() <= MAX_ROUTE_LEN
        && route
            .chars()
            .all(|c| c.is_ascii_graphic() && c != '\\' && c != '#')
        && !route
            .split(['/', '?'])
            .any(|segment| segment == ".." || segment == ".")
}

/// The file name a route's capture gets: `/` is `index`, `/docs/start` is `docs-start`.
fn route_slug(route: &str) -> String {
    let mut slug = String::new();
    for c in route.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "index".to_string()
    } else {
        slug.to_string()
    }
}

/// Each route's slug, two routes never sharing one.
fn route_slugs(routes: &[String]) -> Vec<String> {
    let mut taken = BTreeSet::new();
    routes
        .iter()
        .map(|route| {
            let base = route_slug(route);
            let mut slug = base.clone();
            let mut n = 2;
            while !taken.insert(slug.clone()) {
                slug = format!("{base}-{n}");
                n += 1;
            }
            slug
        })
        .collect()
}

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Where headless Chromium is, looked up the way the web tests do: `ORQ_CHROMIUM`, then `PATH`, then
/// Playwright's browser cache. When there is none, the error says how to install it.
pub fn find_chromium_in(env: &dyn Fn(&str) -> Option<String>) -> Result<PathBuf, String> {
    if let Some(path) = env("ORQ_CHROMIUM").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    if let Some(paths) = env("PATH") {
        for dir in std::env::split_paths(&paths) {
            for name in [
                "chromium",
                "chromium-browser",
                "chrome-headless-shell",
                "google-chrome",
                "google-chrome-stable",
            ] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }
    let cache = env("PLAYWRIGHT_BROWSERS_PATH")
        .filter(|p| !p.is_empty() && p != "0")
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|home| Path::new(&home).join(".cache/ms-playwright")));
    if let Some(cache) = cache {
        let layouts = [
            (
                "chromium_headless_shell-",
                "chrome-headless-shell-linux",
                "chrome-headless-shell",
            ),
            ("chromium-", "chrome-linux", "chrome"),
        ];
        for (install, platform, binary) in layouts {
            let mut found: Vec<PathBuf> = std::fs::read_dir(&cache)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().starts_with(install))
                .flat_map(|entry| {
                    std::fs::read_dir(entry.path())
                        .into_iter()
                        .flatten()
                        .flatten()
                })
                .filter(|entry| entry.file_name().to_string_lossy().starts_with(platform))
                .map(|entry| entry.path().join(binary))
                .filter(|path| path.is_file())
                .collect();
            found.sort();
            if let Some(last) = found.pop() {
                return Ok(last);
            }
        }
    }
    Err("no headless Chromium was found: install Playwright's with `npx playwright install chromium-headless-shell`, \
         put chromium on PATH, or set ORQ_CHROMIUM to its path"
        .to_string())
}

/// Ports this process has handed to a capture still going, so two captures never share one.
static PORTS_IN_USE: Mutex<BTreeSet<u16>> = Mutex::new(BTreeSet::new());

/// A free port, held for one capture until dropped.
struct RunPort(u16);

impl RunPort {
    fn take() -> std::io::Result<RunPort> {
        for _ in 0..64 {
            let port = TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
            let mut in_use = PORTS_IN_USE.lock().unwrap_or_else(|e| e.into_inner());
            if in_use.insert(port) {
                return Ok(RunPort(port));
            }
        }
        Err(std::io::Error::other("no free port"))
    }
}

impl Drop for RunPort {
    fn drop(&mut self) {
        PORTS_IN_USE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

/// Runs one capture round: starts the page, captures every route at every viewport into the run's
/// `design/round-<n>/`, writes `capture.json`, and stops the page and the browser whatever happens.
pub fn capture_design(run: &CaptureRun) -> CaptureOutcome {
    let round_dir = run
        .run_logs
        .join(RUN_DESIGN_DIR)
        .join(format!("round-{}", run.round));
    let commit = head_commit(run.worktree);
    let outcome = capture_round(run, &round_dir, commit.clone());
    let written = match &outcome {
        CaptureOutcome::Captured(record) => {
            let mut value = serde_json::to_value(record).unwrap_or(Value::Null);
            value["outcome"] = json!("captured");
            value["viewports"] = viewports_json(run.viewports);
            value["routes"] = json!(run.routes);
            value
        }
        CaptureOutcome::Blocked(reason) => json!({
            "outcome": "blocked",
            "blocked": reason,
            "round": run.round,
            "commit": commit,
            "viewports": viewports_json(run.viewports),
            "routes": run.routes,
        }),
    };
    let bytes = serde_json::to_vec_pretty(&written).unwrap_or_default();
    if let Err(e) = std::fs::create_dir_all(&round_dir)
        .and_then(|()| std::fs::write(round_dir.join(CAPTURE_FILE), bytes))
    {
        return CaptureOutcome::Blocked(format!(
            "{CAPTURE_FILE} could not be written ({:?})",
            e.kind()
        ));
    }
    outcome
}

fn viewports_json(viewports: &[CaptureViewport]) -> Value {
    Value::Array(
        viewports
            .iter()
            .map(|v| json!({"name": v.name, "width": v.width, "height": v.height}))
            .collect(),
    )
}

fn head_commit(worktree: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .current_dir(worktree)
        .args(["rev-parse", "HEAD"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let sha = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (output.status.success() && !sha.is_empty()).then_some(sha)
}

fn capture_round(run: &CaptureRun, round_dir: &Path, commit: Option<String>) -> CaptureOutcome {
    if round_dir.exists() {
        // A round captured again starts empty, so no capture of an earlier attempt is left in it.
        if let Err(e) = std::fs::remove_dir_all(round_dir) {
            return CaptureOutcome::Blocked(format!(
                "the round's folder could not be cleared ({:?})",
                e.kind()
            ));
        }
    }
    if let Err(e) = std::fs::create_dir_all(round_dir) {
        return CaptureOutcome::Blocked(format!(
            "the round's folder could not be made ({:?})",
            e.kind()
        ));
    }
    let browser_path = match run.browser {
        Some(path) => path.to_path_buf(),
        None => match find_chromium_in(&|name| std::env::var(name).ok()) {
            Ok(path) => path,
            Err(why) => return CaptureOutcome::Blocked(why),
        },
    };
    let port = match RunPort::take() {
        Ok(port) => port,
        Err(e) => return CaptureOutcome::Blocked(format!("no free port for the page ({e})")),
    };
    let server = match start_server(run, round_dir, port.0) {
        Ok(server) => server,
        Err(why) => return CaptureOutcome::Blocked(why),
    };
    let outcome = match wait_until_ready(run.settings, port.0, &server) {
        Err(why) => CaptureOutcome::Blocked(why),
        Ok(()) => match Browser::launch(&browser_path, round_dir) {
            Err(why) => CaptureOutcome::Blocked(why),
            Ok(mut browser) => {
                let outcome = match browser.version() {
                    Err(why) => CaptureOutcome::Blocked(format!("Chromium did not answer: {why}")),
                    Ok(version) => CaptureOutcome::Captured(capture_pages(
                        run,
                        round_dir,
                        &mut browser,
                        port.0,
                        version,
                        commit,
                    )),
                };
                browser.close();
                outcome
            }
        },
    };
    stop(server);
    drop(port);
    outcome
}

/// Starts the start command in the worktree, in a group of its own the run owns, with `PORT` set.
fn start_server(run: &CaptureRun, round_dir: &Path, port: u16) -> Result<TrackedChild, String> {
    let argv = run.settings.start_argv(port)?;
    let Some((program, args)) = argv.split_first() else {
        return Err("design.start_command is empty".to_string());
    };
    let log = File::create(round_dir.join("server.log"))
        .map_err(|e| format!("the server log could not be made ({:?})", e.kind()))?;
    let log_err = log
        .try_clone()
        .map_err(|e| format!("the server log could not be opened ({:?})", e.kind()))?;
    let env = [("PORT".to_string(), port.to_string())];
    let mut command = crate::gate_stage::step_command(program, run.worktree, &env);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    crate::child_groups::spawn(&mut command)
        .map_err(|e| format!("the start command could not be started: {e}"))
}

fn stop(child: TrackedChild) {
    child.end_group();
    let _ = child.reap();
}

/// Polls `ready_path` until it answers 2xx; blocked when the time runs out or the server exits.
fn wait_until_ready(
    settings: &DesignSettings,
    port: u16,
    server: &TrackedChild,
) -> Result<(), String> {
    let deadline = Instant::now() + settings.ready_timeout;
    loop {
        if matches!(server.has_exited(), Ok(true)) {
            return Err(format!(
                "the start command exited before {} answered, so the page is not ready",
                settings.ready_path
            ));
        }
        let addresses: [SocketAddr; 2] = [
            ([127, 0, 0, 1], port).into(),
            (std::net::Ipv6Addr::LOCALHOST, port).into(),
        ];
        if addresses
            .iter()
            .any(|address| answers_2xx(*address, port, &settings.ready_path))
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the page was not ready: {} did not answer 2xx within {}s",
                settings.ready_path,
                settings.ready_timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn answers_2xx(address: SocketAddr, port: u16, path: &str) -> bool {
    let wait = Duration::from_secs(1);
    let Ok(mut stream) = TcpStream::connect_timeout(&address, wait) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(wait));
    let _ = stream.set_write_timeout(Some(wait));
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut head = Vec::new();
    let mut chunk = [0u8; 64];
    while head.len() < 64 && !head.contains(&b'\n') {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => head.extend_from_slice(&chunk[..read]),
        }
    }
    let line = String::from_utf8_lossy(&head);
    let mut words = line.split_whitespace();
    words.next().is_some_and(|w| w.starts_with("HTTP/"))
        && words
            .next()
            .and_then(|code| code.parse::<u16>().ok())
            .is_some_and(|code| (200..300).contains(&code))
}

/// Headless Chromium, spoken to over the DevTools pipe.
struct Browser {
    child: Option<TrackedChild>,
    to: File,
    from: File,
    buffer: Vec<u8>,
    /// How much of `buffer` is known to hold no message end.
    scanned: usize,
    /// The browser closed its end; nothing more will be read.
    closed: bool,
    next_id: u64,
    /// Events kept for the page being captured.
    events: Vec<Value>,
    profile: PathBuf,
}

/// The events the capture reads; every other one is dropped as it arrives.
const KEPT_EVENTS: [&str; 3] = [
    "Page.loadEventFired",
    "Network.responseReceived",
    "Runtime.exceptionThrown",
];

fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // Safety: `fds` has room for the two descriptors pipe2 writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Safety: pipe2 succeeded, so both descriptors are new and ours.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// `fd` again, at 10 or above and closed on exec, so moving it to 3 or 4 in the child clobbers nothing.
fn high_dup(fd: &OwnedFd) -> std::io::Result<OwnedFd> {
    // Safety: plain fcntl on a descriptor we own.
    let new = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    if new < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Safety: fcntl returned a new descriptor that is ours.
    Ok(unsafe { OwnedFd::from_raw_fd(new) })
}

impl Browser {
    fn launch(path: &Path, round_dir: &Path) -> Result<Browser, String> {
        let not_started =
            |e: std::io::Error| format!("Chromium ({}) could not be started: {e}", path.display());
        let profile = std::env::temp_dir().join(format!(
            "orq-capture-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&profile)
                .map_err(not_started)?;
        }
        let (commands_read, commands_write) = pipe().map_err(not_started)?;
        let (replies_read, replies_write) = pipe().map_err(not_started)?;
        let child_in = high_dup(&commands_read).map_err(not_started)?;
        let child_out = high_dup(&replies_write).map_err(not_started)?;
        drop((commands_read, replies_write));
        let log = File::create(round_dir.join("browser.log")).map_err(not_started)?;
        let log_err = log.try_clone().map_err(not_started)?;
        let mut command = std::process::Command::new(path);
        command
            .args([
                "--headless",
                "--disable-gpu",
                "--hide-scrollbars",
                "--mute-audio",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-extensions",
                "--disable-background-networking",
                "--disable-dev-shm-usage",
                "--disable-features=Translate,MediaRouter",
                "--force-device-scale-factor=1",
                "--force-prefers-reduced-motion",
                "--font-render-hinting=none",
                "--lang=en-US",
                "--remote-debugging-pipe",
            ])
            .arg(format!("--user-data-dir={}", profile.display()));
        // The sandbox stays on; only a machine that cannot run it turns it off, on purpose.
        if std::env::var_os("ORQ_CHROMIUM_NO_SANDBOX").is_some_and(|v| v == "1") {
            command.arg("--no-sandbox");
        }
        command
            .arg("about:blank")
            .env("LANG", "en_US.UTF-8")
            .env("LANGUAGE", "en_US")
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(log_err);
        let (child_in_fd, child_out_fd) = (child_in.as_raw_fd(), child_out.as_raw_fd());
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        // Safety: dup2 is async-signal-safe; the descriptors stay open until the spawn returns.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(child_in_fd, 3) < 0 || libc::dup2(child_out_fd, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let spawned = crate::child_groups::spawn(&mut command);
        drop((child_in, child_out));
        let child = match spawned {
            Ok(child) => child,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&profile);
                return Err(not_started(e));
            }
        };
        let from = File::from(replies_read);
        // Safety: fcntl on a descriptor we own; only adds O_NONBLOCK.
        unsafe {
            let flags = libc::fcntl(from.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(from.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        Ok(Browser {
            child: Some(child),
            to: File::from(commands_write),
            from,
            buffer: Vec::new(),
            scanned: 0,
            closed: false,
            next_id: 0,
            events: Vec::new(),
            profile,
        })
    }

    fn version(&mut self) -> Result<String, String> {
        let result = self.call(None, "Browser.getVersion", json!({}), FIRST_ANSWER)?;
        Ok(result["product"].as_str().unwrap_or("unknown").to_string())
    }

    fn next_message(&mut self, deadline: Instant) -> Result<Value, String> {
        loop {
            // Only bytes not looked at before are searched, so a large message read in many
            // chunks is scanned once, not once per chunk.
            if let Some(found) = self.buffer[self.scanned..].iter().position(|b| *b == 0) {
                let end = self.scanned + found;
                self.scanned = 0;
                let message: Vec<u8> = self.buffer.drain(..=end).collect();
                return serde_json::from_slice(&message[..end])
                    .map_err(|_| "the browser sent a message that is not JSON".to_string());
            }
            self.scanned = self.buffer.len();
            if self.closed {
                return Err("the browser closed its connection".to_string());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("the browser did not answer in time".to_string());
            }
            let mut polled = libc::pollfd {
                fd: self.from.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let wait = left.min(Duration::from_millis(200)).as_millis() as libc::c_int;
            // Safety: `polled` is one live pollfd.
            unsafe { libc::poll(&mut polled, 1, wait) };
            let mut chunk = [0u8; 65536];
            loop {
                match self.from.read(&mut chunk) {
                    // What came before the close is still read; the close is reported after it.
                    Ok(0) => {
                        self.closed = true;
                        break;
                    }
                    Ok(read) => {
                        self.buffer.extend_from_slice(&chunk[..read]);
                        if self.buffer.len() > MAX_MESSAGE {
                            return Err("the browser sent a message too large".to_string());
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }
    }

    fn keep(&mut self, message: Value) {
        if message["method"]
            .as_str()
            .is_some_and(|method| KEPT_EVENTS.contains(&method))
        {
            self.events.push(message);
        }
    }

    fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        let mut message = json!({"id": id, "method": method, "params": params});
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        let mut bytes = serde_json::to_vec(&message).map_err(|e| e.to_string())?;
        bytes.push(0);
        self.to
            .write_all(&bytes)
            .map_err(|e| format!("the browser could not be written to: {e}"))?;
        let deadline = Instant::now() + timeout;
        loop {
            let reply = self
                .next_message(deadline)
                .map_err(|why| format!("{method}: {why}"))?;
            if reply["id"].as_u64() == Some(id) {
                if let Some(error) = reply.get("error") {
                    return Err(format!(
                        "{method}: {}",
                        error["message"].as_str().unwrap_or("failed")
                    ));
                }
                return Ok(reply["result"].clone());
            }
            self.keep(reply);
        }
    }

    /// Waits for the event `method` of `session`.
    fn wait_for(&mut self, session: &str, method: &str, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let matches = |m: &Value| m["method"] == method && m["sessionId"] == session;
        if self.events.iter().any(matches) {
            return Ok(());
        }
        loop {
            let message = self.next_message(deadline)?;
            let found = matches(&message);
            self.keep(message);
            if found {
                return Ok(());
            }
        }
    }

    fn session_events(&self, session: &str, method: &str) -> Vec<&Value> {
        self.events
            .iter()
            .filter(|m| m["method"] == method && m["sessionId"] == session)
            .collect()
    }

    fn close(&mut self) {
        if self.child.is_some() {
            let _ = self.call(None, "Browser.close", json!({}), Duration::from_secs(5));
        }
        if let Some(child) = self.child.take() {
            stop(child);
        }
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            stop(child);
        }
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

fn capture_pages(
    run: &CaptureRun,
    round_dir: &Path,
    browser: &mut Browser,
    port: u16,
    browser_version: String,
    commit: Option<String>,
) -> CaptureRecord {
    let slugs = route_slugs(run.routes);
    // Besides the landmarks, every selector the token rules name, for the token check.
    let selectors = token_rule_selectors(&run.run_logs.join(RUN_DESIGN_DIR));
    // The images UX.md lists as data images: an unloaded image is matched against them.
    let data_images = data_image_selectors(&run.run_logs.join(RUN_DESIGN_DIR));
    let mut pages = Vec::new();
    let mut failures = Vec::new();
    for viewport in run.viewports {
        let mut measurements = serde_json::Map::new();
        let folder = round_dir.join(&viewport.name);
        let usable = if !plain_name(&viewport.name) {
            Err(format!(
                "the viewport name {:?} is not a plain name",
                viewport.name
            ))
        } else {
            std::fs::create_dir_all(&folder)
                .map_err(|e| format!("its folder could not be made ({:?})", e.kind()))
        };
        for (route, slug) in run.routes.iter().zip(&slugs) {
            let mut page = PageCapture {
                route: route.clone(),
                viewport: viewport.name.clone(),
                viewport_width: viewport.width,
                viewport_height: viewport.height,
                image: None,
                image_width: None,
                image_height: None,
                failure: None,
            };
            let captured = usable.clone().and_then(|()| {
                if plain_route(route) {
                    capture_page(browser, port, route, viewport, &selectors, &data_images)
                } else {
                    Err("the route is not a plain path".to_string())
                }
            });
            match captured.and_then(|(png, measured)| {
                let (width, height) = png_size(&png).ok_or("the screenshot is not a PNG")?;
                let name = format!("{slug}.png");
                std::fs::write(folder.join(&name), &png)
                    .map_err(|e| format!("the screenshot could not be written ({:?})", e.kind()))?;
                Ok((format!("{}/{name}", viewport.name), width, height, measured))
            }) {
                Ok((image, width, height, measured)) => {
                    page.image = Some(image);
                    page.image_width = Some(width);
                    page.image_height = Some(height);
                    measurements.insert(route.clone(), measured);
                }
                Err(why) => {
                    failures.push(format!("{route} at {}: {why}", viewport.name));
                    page.failure = Some(why);
                }
            }
            pages.push(page);
        }
        if usable.is_ok() {
            let measured = json!({
                "viewport": viewport.name,
                "width": viewport.width,
                "height": viewport.height,
                "routes": measurements,
            });
            let written = serde_json::to_vec_pretty(&measured)
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    std::fs::write(folder.join(MEASUREMENTS_FILE), bytes)
                        .map_err(|e| format!("{:?}", e.kind()))
                });
            if let Err(why) = written {
                failures.push(format!(
                    "{MEASUREMENTS_FILE} at {} could not be written ({why})",
                    viewport.name
                ));
            }
        }
    }
    CaptureRecord {
        round: run.round,
        commit,
        browser_version,
        port,
        pages,
        failures,
    }
}

/// One route at one viewport, in a tab of its own: the PNG and the measurements.
fn capture_page(
    browser: &mut Browser,
    port: u16,
    route: &str,
    viewport: &CaptureViewport,
    selectors: &[String],
    data_images: &[String],
) -> Result<(Vec<u8>, Value), String> {
    let target = browser.call(
        None,
        "Target.createTarget",
        json!({"url": "about:blank"}),
        CALL,
    )?;
    let target_id = target["targetId"]
        .as_str()
        .ok_or("no tab was opened")?
        .to_string();
    let captured = (|| {
        let attached = browser.call(
            None,
            "Target.attachToTarget",
            json!({"targetId": target_id, "flatten": true}),
            CALL,
        )?;
        let session = attached["sessionId"]
            .as_str()
            .ok_or("the tab could not be reached")?
            .to_string();
        let result = capture_in(
            browser,
            &session,
            port,
            route,
            viewport,
            selectors,
            data_images,
        );
        browser
            .events
            .retain(|m| m["sessionId"] != session.as_str());
        result
    })();
    let _ = browser.call(
        None,
        "Target.closeTarget",
        json!({"targetId": target_id}),
        CALL,
    );
    captured
}

fn capture_in(
    browser: &mut Browser,
    session: &str,
    port: u16,
    route: &str,
    viewport: &CaptureViewport,
    selectors: &[String],
    data_images: &[String],
) -> Result<(Vec<u8>, Value), String> {
    let s = Some(session);
    for method in ["Page.enable", "Runtime.enable", "Network.enable"] {
        browser.call(s, method, json!({}), CALL)?;
    }
    browser.call(
        s,
        "Emulation.setDeviceMetricsOverride",
        json!({"width": viewport.width, "height": viewport.height, "deviceScaleFactor": 1, "mobile": false}),
        CALL,
    )?;
    browser.call(
        s,
        "Emulation.setEmulatedMedia",
        json!({"features": [{"name": "prefers-reduced-motion", "value": "reduce"}]}),
        CALL,
    )?;
    // The launch flags already ask for en-US; an older browser without the call still gets it.
    let _ = browser.call(
        s,
        "Emulation.setLocaleOverride",
        json!({"locale": "en-US"}),
        CALL,
    );
    let url = format!("http://localhost:{port}{route}");
    let navigated = browser.call(s, "Page.navigate", json!({"url": url}), PAGE_LOAD)?;
    if let Some(error) = navigated["errorText"].as_str().filter(|e| !e.is_empty()) {
        return Err(format!("the page did not load ({})", short(error)));
    }
    let frame = navigated["frameId"].as_str().unwrap_or("").to_string();
    browser
        .wait_for(session, "Page.loadEventFired", PAGE_LOAD)
        .map_err(|why| {
            format!(
                "the page did not load within {}s ({why})",
                PAGE_LOAD.as_secs()
            )
        })?;
    let status = browser
        .session_events(session, "Network.responseReceived")
        .into_iter()
        .find(|m| m["params"]["type"] == "Document" && m["params"]["frameId"] == frame.as_str())
        .and_then(|m| m["params"]["response"]["status"].as_u64());
    if let Some(status) = status.filter(|s| !(200..400).contains(s)) {
        return Err(format!("the page answered {status}"));
    }
    page_error(browser, session)?;
    let world = browser.call(
        s,
        "Page.createIsolatedWorld",
        json!({"frameId": frame, "worldName": "orq-design-capture"}),
        CALL,
    )?;
    let context = world["executionContextId"]
        .as_u64()
        .ok_or("the page could not be measured")?;
    let evaluated = browser.call(
        s,
        "Runtime.evaluate",
        json!({
            // The selectors last, as one JSON object: their text never reaches another placeholder.
            "expression": MEASURE_SCRIPT
                .replace("ASSETS_BOUND_MS", &ASSETS_BOUND_MS.to_string())
                .replace(
                    "INPUT_JSON",
                    &json!({"selectors": selectors, "data_images": data_images}).to_string()
                ),
            "contextId": context,
            "awaitPromise": true,
            "returnByValue": true,
        }),
        CALL,
    )?;
    if evaluated.get("exceptionDetails").is_some() {
        return Err("the page could not be measured".to_string());
    }
    let measured: Value = evaluated["result"]["value"]
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .ok_or("the page could not be measured")?;
    page_error(browser, session)?;
    let width = measured["document"]["width"]
        .as_f64()
        .map_or(u64::from(viewport.width), |w| w.ceil() as u64)
        .clamp(
            u64::from(viewport.width),
            MAX_PAGE_WIDTH.max(u64::from(viewport.width)),
        );
    let height = measured["document"]["height"]
        .as_f64()
        .map_or(u64::from(viewport.height), |h| h.ceil() as u64)
        .clamp(1, MAX_PAGE_HEIGHT);
    let shot = browser.call(
        s,
        "Page.captureScreenshot",
        json!({
            "format": "png",
            "captureBeyondViewport": true,
            "fromSurface": true,
            "clip": {"x": 0, "y": 0, "width": width, "height": height, "scale": 1},
        }),
        Duration::from_secs(60),
    )?;
    let png = shot["data"]
        .as_str()
        .and_then(base64_decode)
        .ok_or("the screenshot could not be read")?;
    Ok((png, measured))
}

/// The keys of the data images the run's package (and prepared folder) list in UX.md.
fn data_image_selectors(design_dir: &Path) -> Vec<String> {
    crate::design_review::data_image_entries(design_dir)
        .into_iter()
        .map(|(key, _)| key)
        .collect()
}

/// A page error (an uncaught exception) fails the route.
fn page_error(browser: &Browser, session: &str) -> Result<(), String> {
    match browser
        .session_events(session, "Runtime.exceptionThrown")
        .first()
    {
        None => Ok(()),
        Some(event) => {
            let details = &event["params"]["exceptionDetails"];
            let text = details["exception"]["description"]
                .as_str()
                .or_else(|| details["text"].as_str())
                .unwrap_or("an uncaught exception");
            Err(format!("page error: {}", short(text)))
        }
    }
}

/// Page text as recorded: its first line, plain characters, at most 200 of them.
fn short(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect()
}

fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" || &png[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(png[20..24].try_into().ok()?);
    Some((width, height))
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes = text.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            acc |= value(*c)? << (18 - 6 * i);
        }
        match chunk.len() {
            4 => out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8]),
            3 => out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8]),
            2 => out.push((acc >> 16) as u8),
            _ => return None,
        }
    }
    Some(out)
}

/// Run in an isolated world: turns animations and the caret off, waits a bounded time for fonts and
/// images, then measures the landmarks and headings. Returns JSON text.
const MEASURE_SCRIPT: &str = r##"(async function () {
  var style = document.createElement("style");
  style.textContent = "*, *::before, *::after { animation: none !important; transition: none !important; caret-color: transparent !important; scroll-behavior: auto !important; }";
  (document.head || document.documentElement).appendChild(style);
  if (document.getAnimations) {
    document.getAnimations().forEach(function (a) { try { a.finish(); } catch (e) { try { a.cancel(); } catch (e2) {} } });
  }
  var waits = [];
  if (document.fonts && document.fonts.ready) waits.push(document.fonts.ready);
  Array.prototype.forEach.call(document.images, function (img) {
    if (!img.complete) waits.push(new Promise(function (done) {
      img.addEventListener("load", done, {once: true});
      img.addEventListener("error", done, {once: true});
    }));
  });
  await Promise.race([Promise.all(waits), new Promise(function (done) { setTimeout(done, ASSETS_BOUND_MS); })]);
  await Promise.race([
    new Promise(function (done) { requestAnimationFrame(function () { requestAnimationFrame(done); }); }),
    new Promise(function (done) { setTimeout(done, 500); })
  ]);
  function round(v) { return Math.round(v * 100) / 100; }
  var root = document.documentElement;
  var body = document.body;
  var doc = {
    width: Math.max(root.scrollWidth, body ? body.scrollWidth : 0, window.innerWidth),
    height: Math.max(root.scrollHeight, body ? body.scrollHeight : 0, window.innerHeight)
  };
  var props = ["display", "position", "width", "height", "margin-top", "margin-right", "margin-bottom", "margin-left",
    "padding-top", "padding-right", "padding-bottom", "padding-left", "font-family", "font-size", "font-weight",
    "line-height", "letter-spacing", "text-align", "text-transform", "color", "background-color", "border-radius",
    "border-top-width", "border-top-color", "box-shadow", "gap", "flex-direction", "justify-content", "align-items"];
  function measure(element) {
    var rect = element.getBoundingClientRect();
    var computed = getComputedStyle(element);
    var styles = {};
    props.forEach(function (prop) { styles[prop] = computed.getPropertyValue(prop); });
    return {
      box: {x: round(rect.left + window.scrollX), y: round(rect.top + window.scrollY), width: round(rect.width), height: round(rect.height)},
      styles: styles
    };
  }
  var elements = [];
  var counts = {};
  var all = document.querySelectorAll("header, main, nav, footer, h1, h2, h3, button, img");
  for (var i = 0; i < all.length && elements.length < 500; i++) {
    var tag = all[i].tagName.toLowerCase();
    var index = counts[tag] || 0;
    counts[tag] = index + 1;
    var measured = measure(all[i]);
    elements.push({tag: tag, index: index, box: measured.box, styles: measured.styles});
  }
  doc.root_font_size = parseFloat(getComputedStyle(root).fontSize) || 16;
  var input = INPUT_JSON;
  function path(el) {
    var parts = [];
    while (el && el.nodeType === 1 && el !== root && el !== body) {
      var part = el.tagName.toLowerCase();
      if (el.id) { parts.unshift(part + "#" + CSS.escape(el.id)); break; }
      Array.prototype.forEach.call(el.classList, function (c) { part += "." + CSS.escape(c); });
      var parent = el.parentElement;
      if (parent) {
        var same = Array.prototype.filter.call(parent.children, function (c) { return c.tagName === el.tagName; });
        if (same.length > 1) part += ":nth-of-type(" + (same.indexOf(el) + 1) + ")";
      }
      parts.unshift(part);
      el = parent;
    }
    return parts.join(" > ");
  }
  // A data image entry is a CSS selector, or a section's name: its aria-label or heading.
  function named(el, name) {
    if ((el.getAttribute("aria-label") || "").trim().toLowerCase() === name) return true;
    return Array.prototype.some.call(el.children, function (c) {
      return /^H[1-6]$/.test(c.tagName) && c.textContent.trim().toLowerCase() === name;
    });
  }
  function isDataImage(img, key) {
    try { if (img.closest(key)) return true; } catch (e) {}
    var name = key.trim().toLowerCase();
    for (var el = img.parentElement; el && el !== root; el = el.parentElement) {
      if (named(el, name)) return true;
    }
    return false;
  }
  var unloaded = [];
  Array.prototype.forEach.call(document.images, function (img) {
    if (img.naturalWidth > 0 || unloaded.length >= 100) return;
    var dataImage = null;
    for (var k = 0; k < input.data_images.length && dataImage === null; k++) {
      if (isDataImage(img, input.data_images[k])) dataImage = input.data_images[k];
    }
    unloaded.push({selector: path(img), src: img.currentSrc || img.src, data_image: dataImage});
  });
  var selectors = {};
  var selectorCounts = {};
  input.selectors.forEach(function (selector) {
    var found = [];
    try { found = document.querySelectorAll(selector); } catch (e) { found = []; }
    var matched = [];
    for (var j = 0; j < found.length && matched.length < 50; j++) matched.push(measure(found[j]));
    selectors[selector] = matched;
    // How many the selector matched in all: more than were measured is a check left incomplete.
    selectorCounts[selector] = found.length;
  });
  return JSON.stringify({document: doc, elements: elements, selectors: selectors, selector_counts: selectorCounts,
    unloaded_images: unloaded});
})()"##;
