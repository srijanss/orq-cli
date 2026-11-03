//! Telling a person that a run ended: a configured command (an argv list, never shell text)
//! is started with the message in `NOTIFY_MESSAGE`. The message is redacted and bounded, and a
//! notifier that fails, hangs or is missing never changes how the run ended.

use std::path::Path;
use std::time::Duration;

use crate::review_draft::{run_drafter, DraftSession};

const MAX_MESSAGE_CHARS: usize = 1000;

/// Whether `ending` (`review_ready`, `blocked` or `failed`) is one to tell about under `on`
/// (`all`, the default, or `failures`).
pub fn wants_notification(on: Option<&str>, ending: &str) -> bool {
    on != Some("failures") || ending != "review_ready"
}

/// One redacted line saying how the run for `feature` ended, with the review's address if any.
pub fn notification_message(feature: &str, ending: &str, detail: Option<&str>, url: Option<&str>) -> String {
    let mut text = format!("orq: {feature} {ending}");
    match (detail.filter(|d| !d.trim().is_empty()), url) {
        (Some(detail), Some(url)) => text.push_str(&format!(": {detail} ({url})")),
        (Some(detail), None) => text.push_str(&format!(": {detail}")),
        (None, Some(url)) => text.push_str(&format!(": {url}")),
        (None, None) => {}
    }
    let flat: String = orq_core::redact::redact(&text).chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    flat.chars().take(MAX_MESSAGE_CHARS).collect()
}

/// Start the notifier `argv` in `cwd` with `message` in `NOTIFY_MESSAGE` and wait up to `timeout`.
pub fn send_notification(argv: &[String], cwd: &Path, message: &str, timeout: Duration) -> Result<(), String> {
    let (program, args) = argv.split_first().ok_or("the notification command is empty")?;
    let session = DraftSession {
        program: program.clone(),
        args: args.to_vec(),
        env: notifier_env(message),
        cwd: cwd.to_path_buf(),
    };
    run_drafter(&session, timeout).map(|_| ())
}

/// What a desktop or webhook notifier needs to reach its session or network, and nothing else.
const KEPT_VARIABLES: [&str; 12] = [
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_RUNTIME_DIR",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XDG_CURRENT_DESKTOP",
    "LANG",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
];

fn notifier_env(message: &str) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = KEPT_VARIABLES.iter().filter_map(|key| std::env::var(key).ok().map(|v| (key.to_string(), v))).collect();
    env.push(("NOTIFY_MESSAGE".to_string(), message.to_string()));
    env
}
