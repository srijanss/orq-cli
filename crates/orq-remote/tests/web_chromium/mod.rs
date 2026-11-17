//! Shared by the layout tests: runs the built page in a real headless Chromium (jsdom has no layout).
//! A small HTTP server serves `web/index.html`, `app.css` and `app.react.js`, answers `/auth/status`
//! and `/api/...` from the test's fixtures, and 404s `/sw.js` so no service worker runs. A script
//! injected before `</body>` runs the test's steps, measures the page and writes the result as JSON
//! into `<pre id="result">`, which the test reads back from `--dump-dom`.
#![allow(dead_code)]

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// Where Chromium is: `ORQ_TEST_CHROMIUM`, then PATH, then the Playwright cache. None (with the
/// reason printed) when there is none, and the test returns as the jsdom tests do without node.
pub fn binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("ORQ_TEST_CHROMIUM") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for name in ["chromium", "chromium-browser", "chrome-headless-shell"] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let cache = Path::new(&home).join(".cache/ms-playwright");
        let mut found: Vec<PathBuf> = std::fs::read_dir(&cache)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("chromium_headless_shell-"))
            .flat_map(|entry| std::fs::read_dir(entry.path()).into_iter().flatten().flatten())
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("chrome-headless-shell-linux-"))
            .map(|entry| entry.path().join("chrome-headless-shell"))
            .filter(|path| path.is_file())
            .collect();
        found.sort();
        if let Some(last) = found.pop() {
            return Some(last);
        }
    }
    eprintln!("skipped: no Chromium found (set ORQ_TEST_CHROMIUM, put chromium on PATH, or install Playwright's chrome-headless-shell)");
    None
}

/// One fixture: the reply to `method path` (the path includes any query string; a trailing `*`
/// matches any rest).
#[derive(Clone)]
pub struct Reply {
    pub method: &'static str,
    pub path: String,
    pub status: u16,
    pub body: Value,
}

pub fn get(path: &str, body: Value) -> Reply {
    Reply { method: "GET", path: path.to_string(), status: 200, body }
}

pub fn post(path: &str, body: Value) -> Reply {
    Reply { method: "POST", path: path.to_string(), status: 200, body }
}

struct Served {
    replies: Vec<Reply>,
    init: String,
    unanswered: Mutex<Vec<String>>,
}

fn matches(pattern: &str, path: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => path.starts_with(prefix),
        None => pattern == path,
    }
}

fn page(steps: &Value, init: &str) -> String {
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
    let index = std::fs::read_to_string(web.join("index.html")).unwrap();
    let keep = format!("<script>try {{ sessionStorage.setItem(\"orq.token\", \"layout-test-token-0123456789abcdef\"); }} catch (e) {{}}</script><script>{init}</script>");
    let probe = format!("<pre id=\"result\"></pre><script>window.__steps = {};</script><script>{}</script>", steps, PROBE);
    index.replacen("<link rel=\"stylesheet\"", &format!("{keep}\n<link rel=\"stylesheet\""), 1).replacen("</body>", &format!("{probe}\n</body>"), 1)
}

fn respond(mut stream: TcpStream, served: &Served, steps: &Value) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let Ok(read) = stream.read(&mut chunk) else { return };
        if read == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    let length: usize = head
        .lines()
        .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
        .unwrap_or(0);
    while buffer.len() < head_end + length {
        let Ok(read) = stream.read(&mut chunk) else { return };
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let mut first = head.lines().next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_string();
    let target = first.next().unwrap_or("/").to_string();
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
    let (status, kind, body): (u16, &str, Vec<u8>) = if target == "/app.css" {
        (200, "text/css", std::fs::read(web.join("app.css")).unwrap())
    } else if target == "/app.react.js" {
        (200, "text/javascript", std::fs::read(web.join("app.react.js")).unwrap())
    } else if target.starts_with("/auth/") || target.starts_with("/api") {
        match served.replies.iter().find(|reply| reply.method == method && matches(&reply.path, &target)) {
            Some(reply) => (reply.status, "application/json", reply.body.to_string().into_bytes()),
            None if target == "/auth/status" => (200, "application/json", json!({"passkeys": true, "signed_in": false}).to_string().into_bytes()),
            None => {
                served.unanswered.lock().unwrap().push(format!("{method} {target}"));
                (404, "application/json", json!({"error": "not found"}).to_string().into_bytes())
            }
        }
    } else if target == "/sw.js" || target.starts_with("/icons/") || target.starts_with("/manifest") || target.starts_with("/favicon") {
        (404, "text/plain", b"not found".to_vec())
    } else {
        (200, "text/html; charset=utf-8", page(steps, &served.init).into_bytes())
    };
    let reason = if status == 200 { "OK" } else { "Other" };
    let _ = stream.write_all(format!("HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
    let _ = stream.write_all(&body);
}

/// What one page load measured.
pub struct Measured {
    pub result: Value,
    pub unanswered: Vec<String>,
}

/// Opens `path` at `width` x 800 with `replies`, runs `steps` (each `{"click": selector, "text"?: text}`
/// or `{"wait": selector, "text"?: text}`), and returns what the probe measured.
pub fn measure(chromium: &Path, width: u32, path: &str, replies: &[Reply], steps: Value) -> Measured {
    measure_with(chromium, width, path, replies, steps, "")
}

/// As `measure`, with `init` run as a script before the page's own.
pub fn measure_with(chromium: &Path, width: u32, path: &str, replies: &[Reply], steps: Value, init: &str) -> Measured {
    measure_within(chromium, width, path, replies, steps, init, std::time::Duration::from_secs(60))
}

/// A child's output pipe: readable, with a descriptor to poll.
trait ReadFd: std::io::Read {
    fn fd(&self) -> std::os::fd::RawFd;
}

impl<T: std::io::Read + std::os::fd::AsRawFd> ReadFd for T {
    fn fd(&self) -> std::os::fd::RawFd {
        self.as_raw_fd()
    }
}

/// The browser's helpers share its process group; stop them all, so their output ends too.
fn stop_group(child: &std::process::Child) {
    // SAFETY: killpg only sends a signal; the group id is the child's own pid (process_group(0)).
    unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
}

/// How a run reads when the browser exited but a helper that left its group kept the output open.
pub const OUTPUT_LEFT_OPEN: &str = "exited, but its output was still open at the deadline";

/// Output of a browser run that ended, and whose output closed, before the deadline; otherwise why not.
fn run_until(mut command: Command, deadline: std::time::Duration) -> Result<std::process::Output, String> {
    use std::os::unix::process::CommandExt;
    let started = std::time::Instant::now();
    command.process_group(0).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    // A browser script another test thread has just written can be briefly busy (ETXTBSY) while a
    // fork elsewhere still holds its write handle; try again for a moment before giving up.
    let mut tries = 0;
    let mut child = loop {
        match command.spawn() {
            Err(error) if error.raw_os_error() == Some(26) && tries < 50 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(error) => return Err(format!("could not be started: {error}")),
            Ok(child) => break child,
        }
    };
    // Both pipes are read here, without threads, so when this returns they are closed and nothing
    // keeps reading for a helper that outlived the browser.
    let mut pipes: Vec<Box<dyn ReadFd>> = vec![Box::new(child.stdout.take().unwrap()), Box::new(child.stderr.take().unwrap())];
    for pipe in &pipes {
        // SAFETY: fcntl on a descriptor this function owns; only adds O_NONBLOCK.
        unsafe { libc::fcntl(pipe.fd(), libc::F_SETFL, libc::fcntl(pipe.fd(), libc::F_GETFL) | libc::O_NONBLOCK) };
    }
    let mut outputs = [Vec::new(), Vec::new()];
    let mut open = [true, true];
    let mut status = None;
    while status.is_none() || open.iter().any(|o| *o) {
        if started.elapsed() >= deadline {
            break;
        }
        if status.is_none() {
            status = child.try_wait().unwrap();
            if status.is_some() {
                stop_group(&child);
            }
        }
        let mut polled: Vec<libc::pollfd> = pipes.iter().map(|pipe| libc::pollfd { fd: pipe.fd(), events: libc::POLLIN, revents: 0 }).collect();
        // SAFETY: `polled` is a live array of pollfd of the given length.
        unsafe { libc::poll(polled.as_mut_ptr(), polled.len() as libc::nfds_t, 20) };
        for (index, pipe) in pipes.iter_mut().enumerate() {
            if !open[index] {
                continue;
            }
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => {
                        open[index] = false;
                        break;
                    }
                    Ok(read) => outputs[index].extend_from_slice(&chunk[..read]),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }
    }
    drop(pipes);
    stop_group(&child);
    let _ = child.kill();
    let _ = child.wait();
    let Some(status) = status else {
        return Err(format!("timed out after {deadline:?} and was stopped"));
    };
    if open.iter().any(|o| *o) {
        return Err(OUTPUT_LEFT_OPEN.to_string());
    }
    let [stdout, stderr] = outputs;
    Ok(std::process::Output { status, stdout, stderr })
}

pub fn measure_within(chromium: &Path, width: u32, path: &str, replies: &[Reply], steps: Value, init: &str, deadline: std::time::Duration) -> Measured {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let served = Arc::new(Served { replies: replies.to_vec(), init: init.to_string(), unanswered: Mutex::new(Vec::new()) });
    let steps = Arc::new(steps);
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server = {
        let (served, steps, stop) = (served.clone(), steps.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let (served, steps) = (served.clone(), steps.clone());
                        std::thread::spawn(move || respond(stream, &served, &steps));
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
                }
            }
        })
    };
    let profile = orq_test_support::process_temp_dir().join(format!("orq-chromium-{port}"));
    let mut command = Command::new(chromium);
    command
        .args(["--headless", "--no-sandbox", "--disable-gpu", "--hide-scrollbars", "--no-first-run", "--disable-dev-shm-usage"])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--window-size={width},800"))
        .arg("--virtual-time-budget=8000")
        .arg("--dump-dom")
        .arg(format!("http://127.0.0.1:{port}{path}"));
    let output = run_until(command, deadline);
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = server.join();
    let _ = std::fs::remove_dir_all(&profile);
    let unanswered = served.unanswered.lock().unwrap().clone();
    let output = match output {
        Ok(output) => output,
        Err(why) => return Measured { result: json!({"error": format!("{} {why}", chromium.display())}), unanswered },
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Measured { result: json!({"error": format!("{} did not start: {} exit, {}", chromium.display(), output.status, stderr.trim())}), unanswered };
    }
    let dom = String::from_utf8_lossy(&output.stdout);
    let text = dom
        .split("<pre id=\"result\">")
        .nth(1)
        .and_then(|rest| rest.split("</pre>").next())
        .unwrap_or("")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    let result: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({"error": "no result", "stderr": String::from_utf8_lossy(&output.stderr).lines().rev().take(5).collect::<Vec<_>>()}));
    if result.get("error").is_none() && result["clientWidth"].as_u64() != Some(u64::from(width)) {
        let error = format!("the viewport is {} px wide, not the {width} px asked for, so the page was not measured at that width", result["clientWidth"]);
        return Measured { result: json!({"error": error, "measured": result}), unanswered };
    }
    Measured { result, unanswered }
}

const PROBE: &str = r##"
(function () {
  var steps = window.__steps || [];
  function find(step) {
    var all = document.querySelectorAll(step.click || step.wait);
    for (var i = 0; i < all.length; i++) {
      if (step.text === undefined || (all[i].textContent || "").trim().indexOf(step.text) === 0) return all[i];
    }
    return null;
  }
  function clipped(element, width) {
    for (var up = element.parentElement; up && up !== document.body && up !== document.documentElement; up = up.parentElement) {
      var overflow = getComputedStyle(up).overflowX;
      if (overflow !== "visible") return up.getBoundingClientRect().right <= width + 0.5;
    }
    return false;
  }
  function measure() {
    var width = document.documentElement.clientWidth;
    var offenders = [];
    var walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    for (var node = walker.nextNode(); node; node = walker.nextNode()) {
      var element = node.parentElement;
      if (node.nodeValue.trim() === "" || element === null || element.closest("#result, script, style")) continue;
      var range = document.createRange();
      range.selectNodeContents(node);
      var rect = range.getBoundingClientRect();
      if (rect.width === 0 && rect.height === 0) continue;
      if (rect.right > width + 0.5 && !clipped(node, width)) {
        offenders.push({tag: element.tagName.toLowerCase(), cls: String(element.className || ""), text: node.nodeValue.trim().slice(0, 40), right: Math.round(rect.right)});
      }
    }
    var fields = document.body.querySelectorAll("input, select, textarea");
    for (var f = 0; f < fields.length; f++) {
      var box = fields[f].getBoundingClientRect();
      if (box.width > 0 && box.right > width + 0.5 && !clipped(fields[f], width)) offenders.push({tag: fields[f].tagName.toLowerCase(), cls: String(fields[f].className || ""), text: "", right: Math.round(box.right)});
    }
    return {scrollWidth: document.documentElement.scrollWidth, clientWidth: width, offenders: offenders.slice(0, 10), text: document.body.innerText.slice(0, 4000)};
  }
  function linesOf(selector, text) {
    var all = document.querySelectorAll(selector);
    for (var i = 0; i < all.length; i++) {
      if ((all[i].textContent || "").indexOf(text) < 0) continue;
      var range = document.createRange();
      range.selectNodeContents(all[i]);
      var tops = {};
      var rects = range.getClientRects();
      for (var r = 0; r < rects.length; r++) tops[Math.round(rects[r].top)] = true;
      return Object.keys(tops).length;
    }
    return -1;
  }
  var at = 0;
  var waited = 0;
  function next() {
    if (at >= steps.length) {
      setTimeout(function () {
        var result = measure();
        result.lines = {};
        steps.forEach(function (step) { if (step.lines) result.lines[step.lines] = linesOf(step.lines, step.text); });
        document.getElementById("result").textContent = JSON.stringify(result);
      }, 300);
      return;
    }
    var step = steps[at];
    if (step.lines) { at += 1; waited = 0; return next(); }
    var element = find(step);
    if (element === null) {
      waited += 50;
      if (waited > 5000) {
        document.getElementById("result").textContent = JSON.stringify({error: "step " + at + " never found: " + JSON.stringify(step), text: document.body.innerText.slice(0, 3000)});
        return;
      }
      return setTimeout(next, 50);
    }
    if (step.click) element.click();
    at += 1;
    waited = 0;
    setTimeout(next, 100);
  }
  setTimeout(next, 100);
})();
"##;
