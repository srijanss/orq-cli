//! A read-only view of the review inbox for a remote browser.
//!
//! This is the request handler only: it takes an already-parsed request and
//! returns a JSON answer, reading the same stored reports Herdr shows. It has
//! no way to change anything; merging happens at the provider, after
//! following the link in the answer. Every request is read from storage afresh,
//! so a lifecycle change shows on the next request.

use std::net::{IpAddr, Ipv6Addr};

use serde_json::{json, Value};

use crate::review_inbox::{self, InboxDetail};
use crate::storage::{check_run_id, StorageRoots};
use crate::store::Store;

pub struct RemoteConfig {
    /// The shared secret a client sends as `Authorization: Bearer <token>`.
    /// An empty token means nobody is authorised.
    pub token: String,
}

/// Shortest token accepted from the token file.
const MIN_TOKEN_LEN: usize = 32;
/// The rules a token file's metadata must meet: a regular file, owned by the
/// user running the server, and not accessible to anyone else.
pub fn check_token_file_meta(is_file: bool, owner: u32, mode: u32, effective_uid: u32) -> Result<(), String> {
    if !is_file {
        return Err("the remote token file must be a regular file".into());
    }
    if owner != effective_uid {
        return Err("the remote token file must belong to the user running the server".into());
    }
    if mode & 0o077 != 0 {
        return Err("the remote token file must be readable by its owner only (chmod 600)".into());
    }
    Ok(())
}

/// Largest token file read; a bigger one is refused, never cut short.
const MAX_TOKEN_FILE: u64 = 4096;

impl RemoteConfig {
    /// Where the token lives: its own file in Orq's config directory.
    pub fn token_path(roots: &StorageRoots) -> std::path::PathBuf {
        roots.config.join("remote-token")
    }

    /// Like `load`, but a missing token file is created first: 32 random bytes as hex, written
    /// for its owner only, never replacing a file that is already there.
    pub fn load_or_create(roots: &StorageRoots) -> Result<RemoteConfig, String> {
        use std::io::{Read, Write};
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let path = Self::token_path(roots);
        if std::fs::symlink_metadata(&path).is_err() {
            if let Some(dir) = path.parent() {
                std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| format!("cannot create the Orq config directory: {}", e.kind()))?;
            }
            let mut bytes = [0u8; 40];
            std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).map_err(|e| format!("cannot generate a remote token: {}", e.kind()))?;
            let token: String = bytes[..32].iter().map(|b| format!("{b:02x}")).collect();
            // Written whole to a private temporary file, then linked into place without replacing
            // anything: a reader sees the finished token or no file, never a partial one.
            let temp = path.with_file_name(format!(".remote-token.{}.tmp", bytes[32..].iter().map(|b| format!("{b:02x}")).collect::<String>()));
            let written = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&temp)
                .and_then(|mut file| file.write_all(token.as_bytes()).and_then(|()| file.sync_all()));
            let published = written.and_then(|()| std::fs::hard_link(&temp, &path));
            let _ = std::fs::remove_file(&temp);
            match published {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("cannot create the remote token file {}: {}", path.display(), e.kind())),
            }
        }
        Self::load(roots)
    }

    /// Reads the token file. It must be a regular file (not a link) that only
    /// its owner can access, and hold a token of at least 32 characters. An
    /// error never contains the token.
    pub fn load(roots: &StorageRoots) -> Result<RemoteConfig, String> {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let path = Self::token_path(roots);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|e| format!("cannot open the remote token file {}: {}", path.display(), e.kind()))?;
        let meta = file.metadata().map_err(|e| format!("cannot inspect the remote token file: {}", e.kind()))?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        check_token_file_meta(meta.is_file(), meta.uid(), meta.mode(), unsafe { libc::geteuid() })?;
        let mut text = String::new();
        file.take(MAX_TOKEN_FILE + 1).read_to_string(&mut text).map_err(|e| format!("cannot read the remote token file (is it text?): {}", e.kind()))?;
        if text.len() as u64 > MAX_TOKEN_FILE {
            return Err(format!("the remote token file is larger than {MAX_TOKEN_FILE} bytes"));
        }
        let token = text.trim().to_string();
        if token.len() < MIN_TOKEN_LEN || token.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(format!("the remote token must be at least {MIN_TOKEN_LEN} characters with no spaces"));
        }
        Ok(RemoteConfig { token })
    }
}

pub struct Request<'a> {
    pub method: &'a str,
    /// The path, with its query string when it has one (`?state=running&limit=20`).
    pub path: &'a str,
    /// The address the connection came from.
    pub peer: IpAddr,
    pub authorization: Option<&'a str>,
}

pub struct Response {
    pub status: u16,
    pub body: Value,
}

impl Response {
    /// The bytes to put on the wire: JSON, or nothing at all when the body is
    /// absent (a HEAD answer). `Null` means "no body", never the text `null`.
    pub fn body_bytes(&self) -> Vec<u8> {
        if self.body.is_null() {
            Vec::new()
        } else {
            self.body.to_string().into_bytes()
        }
    }
}

fn reply(status: u16, body: Value) -> Response {
    Response { status, body }
}

fn error(status: u16, message: &str) -> Response {
    reply(status, json!({ "error": message }))
}

/// Loopback and the Tailscale ranges only: 100.64.0.0/10 and fd7a:115c:a1e0::/48.
pub fn peer_allowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || (v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1])),
        IpAddr::V6(v6) => v6.is_loopback() || tailscale_v6(&v6) || v6.to_ipv4_mapped().is_some_and(|v4| peer_allowed(IpAddr::V4(v4))),
    }
}

fn tailscale_v6(ip: &Ipv6Addr) -> bool {
    ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0]
}

/// A provider link is passed on only when it is a plain `https` URL with a
/// host, no credentials, no whitespace or control characters, and nothing that
/// could break out of an attribute or a header.
pub fn safe_link(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let bad_char = |c: char| c.is_control() || c.is_whitespace() || !c.is_ascii() || matches!(c, '"' | '\'' | '<' | '>' | '`' | '\\');
    if authority.is_empty() || authority.contains('@') || url.chars().any(bad_char) {
        return None;
    }
    Some(url.to_string())
}

pub fn token_matches(expected: &str, header: Option<&str>) -> bool {
    let Some(given) = header.and_then(|h| h.strip_prefix("Bearer ")) else { return false };
    if expected.is_empty() || given.len() != expected.len() {
        return false;
    }
    given.bytes().zip(expected.bytes()).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}

/// What the server supplies to answer `/api/projects`. Peer, token and method are checked here
/// before it is asked, so a provider never sees a request that was refused.
pub trait ProjectsProvider {
    /// The projects as JSON, or why they cannot be listed.
    fn projects(&self) -> Result<Value, String>;

    /// The review scope of project `id`, `None` when there is no such project.
    fn review_scope(&self, _id: &str) -> Result<Option<ReviewScope>, String> {
        Ok(None)
    }

    /// The ledger features of project `id` as a JSON array, `None` when there is no such project.
    fn features(&self, _id: &str) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// The orchestration runs of project `id`, newest first, as a JSON array; `None` when there is no such project.
    fn runs(&self, _id: &str) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// One run of project `id` as `{run, events, next_cursor}`: its events after the event `after`
    /// (from the start when `None`), one page at most. `None` when the project, the run (of that
    /// project) or the cursor is unknown.
    fn run_timeline(&self, _id: &str, _run: &str, _after: Option<&str>) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// The log files of a run of project `id` (`file` is `None`), or the redacted lines of one file
    /// after the line count `after`, one page at most. `None` when the project, the run (of that
    /// project), the file or the cursor is unknown.
    fn run_logs(&self, _id: &str, _run: &str, _file: Option<&str>, _after: Option<&str>) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// The redacted run settings as JSON, `None` when this provider has none.
    fn settings(&self) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// The harnesses this machine offers and the models each allows, as `[{harness, models}]`;
    /// `None` when this provider offers none, so no agent can be chosen from the web.
    fn setting_options(&self) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// Starts a run of `feature` in project `id`. A provider that cannot start runs does not know the project.
    fn start_run(&self, _id: &str, _feature: &str) -> Result<RunStart, String> {
        Ok(RunStart::Unknown)
    }

    /// Starts a run of `feature` in project `id` with the chosen profiles instead of the launch defaults.
    /// A provider that cannot choose them says so.
    fn start_run_with(&self, _id: &str, _feature: &str, _profiles: &LaunchProfiles) -> Result<RunStart, String> {
        Ok(RunStart::Refused("this server cannot choose the profiles".to_string()))
    }

    /// Pauses, resumes, stops or accepts run `run` of project `id`. An accept is done once its tests have
    /// started; they finish on their own. A provider that cannot does not know the run.
    fn control_run(&self, _id: &str, _run: &str, _control: RunControl) -> Result<RunControlOutcome, String> {
        Ok(RunControlOutcome::Unknown)
    }

    /// Where run `run` of project `id` would continue if resumed now (`Done(next)`), or why it
    /// cannot be; nothing is resumed. A provider that cannot tell does not know the run.
    fn resume_check(&self, _id: &str, _run: &str) -> Result<RunControlOutcome, String> {
        Ok(RunControlOutcome::Unknown)
    }

    /// What a discard of run `run` of project `id` would take with its branch (`Done(warning)`, empty
    /// when nothing would go), without changing anything. A provider that cannot tell does not know the run.
    fn discard_warning(&self, _id: &str, _run: &str) -> Result<RunControlOutcome, String> {
        Ok(RunControlOutcome::Unknown)
    }

    /// Promotes the draft `feature` of project `id` to pending (`Done(feature)`), or says why not in the
    /// ledger's words. A provider that cannot does not know the feature.
    fn promote_feature(&self, _id: &str, _feature: &str) -> Result<RunControlOutcome, String> {
        Ok(RunControlOutcome::Unknown)
    }

    /// Records that the paused run `run` of project `id` continues `role` ("implementer" or "reviewer") on the
    /// model profile `profile` when it resumes (`Done(run)`), or why not. A provider that cannot does not know the run.
    fn switch_model(&self, _id: &str, _run: &str, _role: &str, _profile: &str) -> Result<RunControlOutcome, String> {
        Ok(RunControlOutcome::Unknown)
    }

    /// The model profiles a run of project `id` can be started or switched with, and the ones the popup's
    /// launch picker preselects, as `{profiles, implementer, reviewer}`; `None` when there is no such project.
    fn launch_profiles(&self, _id: &str) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// The editable fields of `feature` of project `id`, as `{fields, version}`, for the web's editor; the
    /// version names the ledger as it was read. `None` when there is no such project or feature.
    fn feature_fields(&self, _id: &str, _feature: &str) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// Saves the web editor's `fields` as a new feature of project `id` (`feature` is `None`) or as the
    /// fields of `feature`, if the ledger still has the `version` the editor read (an add may send none).
    /// A provider that cannot does not know the project.
    fn save_feature(&self, _id: &str, _feature: Option<&str>, _fields: &FeatureInput, _version: Option<&str>) -> Result<FeatureSave, String> {
        Ok(FeatureSave::Unknown)
    }

    /// Discards run `run` of project `id` the way the popup's Discard and Clear do. A provider that
    /// cannot does not know the run.
    fn discard_run(&self, _id: &str, _run: &str) -> Result<RunDiscard, String> {
        Ok(RunDiscard::Unknown)
    }
}

/// The fields the web's feature editor sends; a field left out (or null) stays as it is on an edit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeatureInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub depends_on: Option<Vec<String>>,
    pub parallelizable: Option<bool>,
    pub test_file: Option<String>,
    pub target_files: Option<Vec<String>>,
}

/// How a save of a feature ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeatureSave {
    Saved,
    /// The ledger refused the values; nothing was written. Safe to show.
    Invalid(String),
    /// The ledger changed since the editor read it, or another writer holds it; nothing was written. Safe to show.
    Conflict(String),
    /// No such project or feature.
    Unknown,
}

/// How a request to discard a run ended.
pub enum RunDiscard {
    /// The run is gone. `note` says, safe to show, which features are pending again and what was kept
    /// or deleted; `cleared` is true when the run's work was on its target branch, so its features stay.
    Discarded { note: String, cleared: bool },
    /// Why this run may not be discarded now; safe to show.
    Refused(String),
    /// There is no such project or run.
    Unknown,
}

/// What the web may ask of a run that already exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunControl {
    Pause,
    Resume,
    Stop,
    Accept,
}

/// How a request to pause or resume a run ended.
pub enum RunControlOutcome {
    /// The run now concerned: the paused run, or the run that carries on after a resume.
    Done(String),
    /// Why this run may not be paused or resumed now; safe to show.
    Refused(String),
    /// There is no such project or run.
    Unknown,
}

/// How a request to start a run ended.
pub enum RunStart {
    /// The run that began, by id.
    Started(String),
    /// Why this feature may not start a run now; safe to show.
    Refused(String),
    /// There is no such project or feature.
    Unknown,
}

/// The implementer and reviewer profiles a run is started with, as the popup's launch picker chooses them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchProfiles {
    pub implementer: String,
    pub reviewer: String,
}

/// The run settings of the config file at `path`, as `{implementer, reviewer, models}`, for the web.
/// Only the harness, model and effort of each agent leave, and each through the redactor; every
/// other key (tokens, environment, credential paths, the remote section) is left behind. A missing
/// file is empty settings; a file that cannot be parsed is an error that does not name the file.
pub fn public_settings(path: &std::path::Path) -> Result<Value, String> {
    let text = crate::remote_settings::read_config(path).map_err(|_| "the settings could not be read".to_string())?;
    let doc: toml_edit::DocumentMut = text.parse().map_err(|_| "the settings could not be read".to_string())?;
    let field = |table: &dyn toml_edit::TableLike, key: &str| table.get(key).and_then(|item| item.as_str()).map(|value| Value::String(crate::redact::redact(value))).unwrap_or(Value::Null);
    let agent = |table: &dyn toml_edit::TableLike| json!({"harness": field(table, "harness"), "model": field(table, "model"), "effort": field(table, "effort")});
    let section = |name: &str| doc.get(name).and_then(|item| item.as_table_like()).map(agent).unwrap_or(Value::Null);
    let models: Vec<Value> = doc.get("models").and_then(|item| item.as_table_like()).map(|table| table.iter().filter_map(|(name, item)| item.as_table_like().map(|t| json!({"name": crate::redact::redact(name), "harness": field(t, "harness"), "model": field(t, "model"), "effort": field(t, "effort")}))).collect()).unwrap_or_default();
    // Off unless this one setting is a real `true`; `remote_control` never turns it on.
    let remote_actions = doc.get("remote_actions").and_then(|item| item.as_bool()) == Some(true);
    Ok(json!({"implementer": section("implementer"), "reviewer": section("reviewer"), "models": models, "remote_actions": remote_actions, "version": crate::remote_settings::version_of(&text)}))
}

/// How the review store knows a project: the id its reports are saved under and its display name.
pub struct ReviewScope {
    pub review_key: String,
    pub name: String,
}

pub fn handle(roots: &StorageRoots, config: &RemoteConfig, project_id: &str, project_name: &str, request: &Request) -> Response {
    answer(roots, config, project_id, project_name, request, b"", None, None)
}

/// Like [`handle`], and `/api/projects` is answered by `provider`.
pub fn handle_with(roots: &StorageRoots, config: &RemoteConfig, project_id: &str, project_name: &str, request: &Request, provider: &dyn ProjectsProvider) -> Response {
    answer(roots, config, project_id, project_name, request, b"", Some(provider), None)
}

/// The most an action's request body may hold; the transport reads no more than this.
pub const MAX_ACTION_BODY: usize = 4096;

/// Like [`handle_with`], with the request's body, which only the action routes look at.
pub fn handle_with_body(roots: &StorageRoots, config: &RemoteConfig, project_id: &str, project_name: &str, request: &Request, body: &[u8], provider: &dyn ProjectsProvider) -> Response {
    answer(roots, config, project_id, project_name, request, body, Some(provider), None)
}

/// A check that an assertion from the page confirms turning remote actions on, given only for a
/// request from a passkey session; it spends the challenge the assertion answers.
pub type StepUp<'a> = &'a dyn Fn(&Value) -> bool;

/// Like [`handle_with_body`], from a passkey session whose step-up assertions `confirm` checks.
#[allow(clippy::too_many_arguments)]
pub fn handle_with_confirm(roots: &StorageRoots, config: &RemoteConfig, project_id: &str, project_name: &str, request: &Request, body: &[u8], provider: &dyn ProjectsProvider, confirm: Option<StepUp>) -> Response {
    answer(roots, config, project_id, project_name, request, body, Some(provider), confirm)
}

/// Why an action's body is refused, if it is: it must be empty or a JSON object with only `allowed` fields.
fn body_refusal(body: &[u8], allowed: &[&str]) -> Option<Response> {
    if body.len() > MAX_ACTION_BODY {
        return Some(error(413, "the request body is too large"));
    }
    if body.iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(fields)) if fields.keys().all(|key| allowed.contains(&key.as_str())) => None,
        _ => Some(error(400, "the request body is not one this action takes")),
    }
}

#[allow(clippy::too_many_arguments)]
fn answer(roots: &StorageRoots, config: &RemoteConfig, project_id: &str, project_name: &str, request: &Request, body: &[u8], provider: Option<&dyn ProjectsProvider>, confirm: Option<StepUp>) -> Response {
    let mut response = route(roots, config, project_id, project_name, request, body, provider, confirm);
    if request.method == "HEAD" {
        // Same status as GET, never a body.
        response.body = Value::Null;
    }
    response
}

/// The project and feature of `/api/projects/{id}/features/{name}/runs`, when the path has that shape.
fn run_start_target(path: &str) -> Option<(&str, &str)> {
    feature_target(path, "runs")
}

/// The project, run and control of `/api/projects/{id}/runs/{run}/pause`, `.../resume`, `.../stop` or `.../accept`, when the path has that shape.
fn run_control_target(path: &str) -> Option<(&str, &str, RunControl)> {
    let mut parts = path.strip_prefix("/api/projects/")?.split('/');
    let (id, runs, run, action) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if runs != "runs" || id.is_empty() || run.is_empty() || parts.next().is_some() {
        return None;
    }
    match action {
        "pause" => Some((id, run, RunControl::Pause)),
        "resume" => Some((id, run, RunControl::Resume)),
        "stop" => Some((id, run, RunControl::Stop)),
        "accept" => Some((id, run, RunControl::Accept)),
        _ => None,
    }
}

/// Whether the settings switch remote actions on; anything unreadable counts as off.
fn remote_actions_on(roots: &StorageRoots) -> bool {
    public_settings(&roots.config_file()).ok().and_then(|settings| settings["remote_actions"].as_bool()) == Some(true)
}

/// Turns remote actions off or on for the web, while the settings version presented is still current,
/// and leaves an audit line for the attempt. Turning them on needs a passkey session (`confirm`) and an
/// assertion in the body that it accepts.
fn set_remote_actions(roots: &StorageRoots, body: &[u8], confirm: Option<StepUp>) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    use crate::remote_settings::{write_if_version, SettingsWriteError};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let audit = |outcome, reason: Option<&str>| {
        let _ = record(roots, &Attempt { identity: "web", action: "settings.remote_actions", project: "settings", run: None, outcome, reason }, now);
    };
    if let Some(refusal) = body_refusal(body, &["on", "version", "assertion"]) {
        audit(Outcome::Refused, Some("bad request body"));
        return refusal;
    }
    let fields = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
    let (Some(on), Some(version)) = (fields["on"].as_bool(), fields["version"].as_str()) else {
        audit(Outcome::Refused, Some("bad request body"));
        return error(400, "the request body is not one this action takes");
    };
    if on {
        let Some(confirm) = confirm else {
            audit(Outcome::Refused, Some("needs a passkey sign-in"));
            return error(403, "Turning remote actions on needs a passkey sign-in");
        };
        if !confirm(&fields["assertion"]) {
            audit(Outcome::Refused, Some("passkey check failed"));
            return error(403, "The passkey check did not pass");
        }
    }
    match write_if_version(roots, version, |doc| doc["remote_actions"] = toml_edit::value(on)) {
        Ok(()) => {
            audit(Outcome::Allowed, None);
            reply(200, json!({ "remote_actions": on }))
        }
        Err(conflict @ SettingsWriteError::Conflict) => {
            audit(Outcome::Refused, Some("settings changed"));
            error(conflict.status(), &conflict.to_string())
        }
        Err(failed) => {
            audit(Outcome::Refused, Some(&failed.to_string()));
            error(failed.status(), &failed.to_string())
        }
    }
}

/// Whether `harness` with `model` is one of `options` (`[{harness, models}]`), exactly as listed.
fn offered(options: &Value, harness: &str, model: &str) -> bool {
    options.as_array().is_some_and(|options| options.iter().any(|option| option["harness"] == harness && option["models"].as_array().is_some_and(|models| models.iter().any(|offered| offered == model))))
}

/// Sets the implementer's and reviewer's harness and model for the web, if actions are on, from the
/// provider's options only and while the settings version presented is still current; a role left out
/// or null is not touched. Leaves an audit line for the attempt.
fn set_agents(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    use crate::remote_settings::{write_if_version, SettingsWriteError};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let audit = |outcome, reason: Option<&str>| {
        let _ = record(roots, &Attempt { identity: "web", action: "settings.agents", project: "settings", run: None, outcome, reason }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, Some("remote actions are off"));
        return error(404, "not found");
    }
    let bad_body = || {
        audit(Outcome::Refused, Some("bad request body"));
        error(400, "the request body is not one this action takes")
    };
    if let Some(refusal) = body_refusal(body, &["implementer", "reviewer", "version"]) {
        audit(Outcome::Refused, Some("bad request body"));
        return refusal;
    }
    let fields = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
    let Some(version) = fields["version"].as_str() else { return bad_body() };
    let mut chosen: Vec<(&str, &str, &str)> = Vec::new();
    for role in ["implementer", "reviewer"] {
        match &fields[role] {
            Value::Null => {}
            Value::Object(agent) if agent.keys().all(|key| key == "harness" || key == "model") => match (agent.get("harness").and_then(Value::as_str), agent.get("model").and_then(Value::as_str)) {
                (Some(harness), Some(model)) => chosen.push((role, harness, model)),
                _ => return bad_body(),
            },
            _ => return bad_body(),
        }
    }
    if chosen.is_empty() {
        return bad_body();
    }
    let options = match provider.map(|provider| provider.setting_options()) {
        Some(Ok(Some(options))) => options,
        Some(Err(_)) => {
            audit(Outcome::Refused, Some("the options could not be read"));
            return error(500, "the options could not be read");
        }
        _ => {
            audit(Outcome::Refused, Some("no options to choose from"));
            return error(404, "not found");
        }
    };
    if !chosen.iter().all(|(_, harness, model)| offered(&options, harness, model)) {
        audit(Outcome::Refused, Some("not an offered harness or model"));
        return error(422, "That harness or model is not one this machine offers");
    }
    // A role kept as something other than a table is left for a person to fix, and then nothing is
    // written, so a save never lands for one role and not the other.
    let mut not_a_table = false;
    let written = write_if_version(roots, version, |doc| {
        not_a_table = chosen.iter().any(|(role, _, _)| doc.get(role).is_some_and(|item| item.as_table_like().is_none()));
        if not_a_table {
            return;
        }
        for (role, harness, model) in &chosen {
            let table = doc.entry(role).or_insert_with(toml_edit::table);
            if let Some(table) = table.as_table_like_mut() {
                table.insert("harness", toml_edit::value(*harness));
                table.remove("profile");
                table.insert("model", toml_edit::value(*model));
            }
        }
    });
    match written {
        Ok(()) if not_a_table => {
            audit(Outcome::Refused, Some("a role in the settings is not a table"));
            error(500, "the settings could not be written")
        }
        Ok(()) => {
            audit(Outcome::Allowed, None);
            match public_settings(&roots.config_file()) {
                Ok(settings) => reply(200, json!({ "settings": settings })),
                Err(why) => error(500, &why),
            }
        }
        Err(conflict @ SettingsWriteError::Conflict) => {
            audit(Outcome::Refused, Some("settings changed"));
            error(conflict.status(), &conflict.to_string())
        }
        Err(failed) => {
            audit(Outcome::Refused, Some(&failed.to_string()));
            error(failed.status(), &failed.to_string())
        }
    }
}

/// Starts a run for the web, if actions are on, and leaves an audit line for the attempt.
fn start_run(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, feature: &str, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let audit = |outcome, run: Option<&str>, reason: Option<&str>| {
        let _ = record(roots, &Attempt { identity: "web", action: "run.start", project: id, run, outcome, reason }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, None, Some("remote actions are off"));
        return error(404, "not found");
    }
    if !plain_feature_name(feature) {
        audit(Outcome::Refused, None, Some("not a feature name"));
        return error(404, "not found");
    }
    let chosen = match launch_choice(body) {
        Ok(chosen) => chosen,
        Err(refusal) => {
            audit(Outcome::Refused, None, Some("bad request body"));
            return refusal;
        }
    };
    let started = provider.map(|provider| match &chosen {
        Some(profiles) => provider.start_run_with(id, feature, profiles),
        None => provider.start_run(id, feature),
    });
    let named = chosen.as_ref().map(|profiles| format!("implementer {}, reviewer {}", profiles.implementer, profiles.reviewer));
    match started {
        Some(Ok(RunStart::Started(run))) => {
            audit(Outcome::Allowed, Some(&run), named.as_deref());
            reply(202, json!({ "run": run }))
        }
        Some(Ok(RunStart::Refused(why))) => {
            audit(Outcome::Refused, None, Some(&why));
            reply(409, json!({ "error": why }))
        }
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(_)) => {
            audit(Outcome::Refused, None, Some("the run could not be started"));
            error(500, "the run could not be started")
        }
        _ => {
            audit(Outcome::Refused, None, Some("no such project or feature"));
            error(404, "not found")
        }
    }
}

/// The profiles a start body chooses: none for an empty body or `{}`, which keep the launch defaults, or both
/// an implementer and a reviewer, each a non-empty name, and nothing else.
fn launch_choice(body: &[u8]) -> Result<Option<LaunchProfiles>, Response> {
    if let Some(refusal) = body_refusal(body, &["implementer", "reviewer"]) {
        return Err(refusal);
    }
    let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(body) else { return Ok(None) };
    if fields.is_empty() {
        return Ok(None);
    }
    let named = |key: &str| fields.get(key).and_then(Value::as_str).filter(|name| !name.trim().is_empty()).map(str::to_string);
    match (named("implementer"), named("reviewer")) {
        (Some(implementer), Some(reviewer)) => Ok(Some(LaunchProfiles { implementer, reviewer })),
        _ => Err(error(400, "the request body is not one this action takes")),
    }
}

/// Whether `feature` can only be a feature name, never a path.
fn plain_feature_name(feature: &str) -> bool {
    feature.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) && !feature.starts_with('.')
}

/// The project and feature of `/api/projects/{id}/features/{name}/{leaf}`, when the path has that shape.
fn feature_target<'a>(path: &'a str, leaf: &str) -> Option<(&'a str, &'a str)> {
    match path.strip_prefix("/api/projects/")?.split('/').collect::<Vec<_>>().as_slice() {
        [id, "features", feature, last] if *last == leaf && !id.is_empty() && !feature.is_empty() => Some((id, feature)),
        _ => None,
    }
}

/// Promotes a draft feature for the web, if actions are on, and leaves an audit line for the attempt as
/// feature.promote; each line names the feature, since an audit line has no feature column.
fn promote_feature(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, feature: &str, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let failed = "the feature could not be promoted";
    let audit = |outcome, reason: &str| {
        let _ = record(roots, &Attempt { identity: "web", action: "feature.promote", project: id, run: None, outcome, reason: Some(reason) }, now);
    };
    if !plain_feature_name(feature) {
        audit(Outcome::Refused, "not a feature name");
        return error(404, "not found");
    }
    let refused = |why: &str| audit(Outcome::Refused, &format!("{feature}: {why}"));
    if !remote_actions_on(roots) {
        refused("remote actions are off");
        return error(404, "not found");
    }
    if let Some(refusal) = body_refusal(body, &[]) {
        refused("bad request body");
        return refusal;
    }
    match provider.map(|provider| provider.promote_feature(id, feature)) {
        Some(Ok(RunControlOutcome::Done(_))) => {
            audit(Outcome::Allowed, feature);
            reply(200, json!({ "feature": feature, "status": "pending" }))
        }
        Some(Ok(RunControlOutcome::Refused(why))) => {
            refused(&why);
            reply(409, json!({ "error": why }))
        }
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(_)) => {
            refused(failed);
            error(500, failed)
        }
        _ => {
            refused("no such project or feature");
            error(404, "not found")
        }
    }
}

/// The project and feature of `/api/projects/{id}/features/{name}`, when the path has that shape.
fn feature_path(path: &str) -> Option<(&str, &str)> {
    match path.strip_prefix("/api/projects/")?.split('/').collect::<Vec<_>>().as_slice() {
        [id, "features", feature] if !id.is_empty() && !feature.is_empty() => Some((id, feature)),
        _ => None,
    }
}

/// The editor's fields and the version they were read with, from a save body `{fields, version}`. Only the
/// editor's fields are taken, each of its own type; an add must name the feature, and an edit must send a
/// version and may not rename the feature.
fn feature_save_body(body: &[u8], feature: Option<&str>) -> Result<(FeatureInput, Option<String>), Response> {
    let bad = || error(400, "the request body is not one this action takes");
    if let Some(refusal) = body_refusal(body, &["fields", "version"]) {
        return Err(refusal);
    }
    let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(body) else { return Err(bad()) };
    let Some(Value::Object(fields)) = body.get("fields") else { return Err(bad()) };
    const FIELDS: [&str; 6] = ["name", "description", "depends_on", "parallelizable", "test_file", "target_files"];
    if !fields.keys().all(|key| FIELDS.contains(&key.as_str())) {
        return Err(bad());
    }
    let text = |key: &str| match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(bad()),
    };
    let names = |key: &str| match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items.iter().map(|item| item.as_str().map(str::to_string).ok_or_else(bad)).collect::<Result<Vec<_>, _>>().map(Some),
        _ => Err(bad()),
    };
    let parallelizable = match fields.get("parallelizable") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(value)) => Some(*value),
        _ => return Err(bad()),
    };
    let input = FeatureInput {
        name: text("name")?,
        description: text("description")?,
        depends_on: names("depends_on")?,
        parallelizable,
        test_file: text("test_file")?,
        target_files: names("target_files")?,
    };
    let version = match body.get("version") {
        None | Some(Value::Null) => None,
        Some(Value::String(version)) if !version.is_empty() => Some(version.clone()),
        _ => return Err(bad()),
    };
    let fits = match feature {
        None => input.name.as_deref().is_some_and(|name| !name.trim().is_empty()),
        Some(feature) => version.is_some() && input.name.as_deref().is_none_or(|name| name == feature),
    };
    if fits { Ok((input, version)) } else { Err(bad()) }
}

/// Adds (`feature` is `None`) or edits a feature for the web's editor, if actions are on, and leaves an audit
/// line for the attempt as feature.add or feature.edit, naming the feature once it is known.
fn save_feature(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, feature: Option<&str>, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let action = if feature.is_some() { "feature.edit" } else { "feature.add" };
    let failed = "the feature could not be saved";
    let audit = |outcome, reason: &str| {
        let _ = record(roots, &Attempt { identity: "web", action, project: id, run: None, outcome, reason: Some(reason) }, now);
    };
    let named = |name: Option<&str>, why: &str| name.map_or_else(|| why.to_string(), |name| format!("{name}: {why}"));
    if feature.is_some_and(|feature| !plain_feature_name(feature)) {
        audit(Outcome::Refused, "not a feature name");
        return error(404, "not found");
    }
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, &named(feature, "remote actions are off"));
        return error(404, "not found");
    }
    let (input, version) = match feature_save_body(body, feature) {
        Ok(parsed) => parsed,
        Err(refusal) => {
            audit(Outcome::Refused, &named(feature, "bad request body"));
            return refusal;
        }
    };
    let name = feature.map(str::to_string).or_else(|| input.name.clone()).unwrap_or_default();
    let refused = |why: &str| audit(Outcome::Refused, &named(Some(&name), why));
    match provider.map(|provider| provider.save_feature(id, feature, &input, version.as_deref())) {
        Some(Ok(FeatureSave::Saved)) => {
            audit(Outcome::Allowed, &name);
            reply(200, json!({ "feature": name }))
        }
        Some(Ok(FeatureSave::Invalid(why))) => {
            refused(&why);
            reply(422, json!({ "error": why }))
        }
        Some(Ok(FeatureSave::Conflict(why))) => {
            refused(&why);
            reply(409, json!({ "error": why }))
        }
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(_)) => {
            refused(failed);
            error(500, failed)
        }
        _ => {
            refused("no such project or feature");
            error(404, "not found")
        }
    }
}

/// Pauses, resumes, stops or accepts a run for the web, if actions are on, and leaves an audit line for the attempt.
fn control_run(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, run: &str, control: RunControl, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let (action, failed) = match control {
        RunControl::Pause => ("run.pause", "the run could not be paused"),
        RunControl::Resume => ("run.resume", "the run could not be resumed"),
        RunControl::Stop => ("run.stop", "the run could not be stopped"),
        RunControl::Accept => ("run.accept", "the run could not be accepted"),
    };
    let audit = |outcome, run: Option<&str>, reason: Option<&str>| {
        let _ = record(roots, &Attempt { identity: "web", action, project: id, run, outcome, reason }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, None, Some("remote actions are off"));
        return error(404, "not found");
    }
    let plain = plain_run_id(run);
    if !plain {
        audit(Outcome::Refused, None, Some("not a run id"));
        return error(404, "not found");
    }
    if let Some(refusal) = body_refusal(body, &[]) {
        audit(Outcome::Refused, Some(run), Some("bad request body"));
        return refusal;
    }
    match provider.map(|provider| provider.control_run(id, run, control)) {
        Some(Ok(RunControlOutcome::Done(now_run))) => {
            audit(Outcome::Allowed, Some(run), None);
            reply(202, json!({ "run": now_run }))
        }
        Some(Ok(RunControlOutcome::Refused(why))) => {
            audit(Outcome::Refused, Some(run), Some(&why));
            reply(409, json!({ "error": why }))
        }
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(_)) => {
            audit(Outcome::Refused, Some(run), Some(failed));
            error(500, failed)
        }
        _ => {
            audit(Outcome::Refused, None, Some("no such project or run"));
            error(404, "not found")
        }
    }
}

/// Whether `run` can only be a run id, never a path.
fn plain_run_id(run: &str) -> bool {
    run.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) && !run.starts_with('.')
}

/// The project and run of `/api/projects/{id}/runs/{run}/{leaf}`, when the path has that shape.
fn run_target<'a>(path: &'a str, leaf: &str) -> Option<(&'a str, &'a str)> {
    let mut parts = path.strip_prefix("/api/projects/")?.split('/');
    let (id, runs, run, last) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    (runs == "runs" && last == leaf && !id.is_empty() && !run.is_empty() && parts.next().is_none()).then_some((id, run))
}

/// What a discard would take with the branch, for the web's question before Discard. It changes
/// nothing, so it is not audited, but it is there only while actions are on.
fn discard_warning(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, run: &str) -> Response {
    if !remote_actions_on(roots) || !plain_run_id(run) {
        return error(404, "not found");
    }
    match provider.map(|provider| provider.discard_warning(id, run)) {
        Some(Ok(RunControlOutcome::Done(warning))) => reply(200, json!({ "run": run, "warning": warning })),
        Some(Ok(RunControlOutcome::Refused(why))) => reply(409, json!({ "error": why })),
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(_)) => error(500, "the run could not be checked"),
        _ => error(404, "not found"),
    }
}

/// Discards a run for the web, if actions are on, and leaves an audit line for the attempt: run.clear
/// when the run's work was on its target branch, run.discard otherwise.
fn discard_run(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, run: &str, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let failed = "the run could not be discarded";
    let audit = |action, outcome, run: Option<&str>, reason: Option<&str>| {
        let _ = record(roots, &Attempt { identity: "web", action, project: id, run, outcome, reason }, now);
    };
    if !remote_actions_on(roots) {
        audit("run.discard", Outcome::Refused, None, Some("remote actions are off"));
        return error(404, "not found");
    }
    if !plain_run_id(run) {
        audit("run.discard", Outcome::Refused, None, Some("not a run id"));
        return error(404, "not found");
    }
    if let Some(refusal) = body_refusal(body, &[]) {
        audit("run.discard", Outcome::Refused, Some(run), Some("bad request body"));
        return refusal;
    }
    match provider.map(|provider| provider.discard_run(id, run)) {
        Some(Ok(RunDiscard::Discarded { note, cleared })) => {
            audit(if cleared { "run.clear" } else { "run.discard" }, Outcome::Allowed, Some(run), None);
            reply(202, json!({ "run": run, "note": note.trim(), "cleared": cleared }))
        }
        Some(Ok(RunDiscard::Refused(why))) => {
            audit("run.discard", Outcome::Refused, Some(run), Some(&why));
            reply(409, json!({ "error": why }))
        }
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(_)) => {
            audit("run.discard", Outcome::Refused, Some(run), Some(failed));
            error(500, failed)
        }
        _ => {
            audit("run.discard", Outcome::Refused, None, Some("no such project or run"));
            error(404, "not found")
        }
    }
}

/// The role and profile of a model switch body, `{"role": "implementer"|"reviewer", "profile": "<name>"}` and
/// nothing else; `None` for any other body.
fn model_switch(body: &[u8]) -> Option<(String, String)> {
    if body.len() > MAX_ACTION_BODY {
        return None;
    }
    let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(body) else { return None };
    let role = fields.get("role")?.as_str().filter(|role| matches!(*role, "implementer" | "reviewer"))?;
    let profile = fields.get("profile")?.as_str().filter(|profile| !profile.trim().is_empty())?;
    (fields.len() == 2).then(|| (role.to_string(), profile.to_string()))
}

/// Switches the model of a paused run for the web, if actions are on, and leaves an audit line for the
/// attempt as run.model; an allowed one says which role went to which profile.
fn switch_model(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, run: &str, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let failed = "the model could not be switched";
    let audit = |outcome, run: Option<&str>, reason: Option<&str>| {
        let _ = record(roots, &Attempt { identity: "web", action: "run.model", project: id, run, outcome, reason }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, None, Some("remote actions are off"));
        return error(404, "not found");
    }
    if !plain_run_id(run) {
        audit(Outcome::Refused, None, Some("not a run id"));
        return error(404, "not found");
    }
    let Some((role, profile)) = model_switch(body) else {
        audit(Outcome::Refused, Some(run), Some("bad request body"));
        return if body.len() > MAX_ACTION_BODY { error(413, "the request body is too large") } else { error(400, "the request body is not one this action takes") };
    };
    match provider.map(|provider| provider.switch_model(id, run, &role, &profile)) {
        Some(Ok(RunControlOutcome::Done(_))) => {
            audit(Outcome::Allowed, Some(run), Some(&format!("{role} on {profile}")));
            reply(202, json!({ "run": run, "role": role, "profile": profile }))
        }
        Some(Ok(RunControlOutcome::Refused(why))) => {
            audit(Outcome::Refused, Some(run), Some(&why));
            reply(409, json!({ "error": why }))
        }
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(_)) => {
            audit(Outcome::Refused, Some(run), Some(failed));
            error(500, failed)
        }
        _ => {
            audit(Outcome::Refused, None, Some("no such project or run"));
            error(404, "not found")
        }
    }
}

/// The project of `/api/projects/{id}/{leaf}`, when the path has that shape.
fn project_target<'a>(path: &'a str, leaf: &str) -> Option<&'a str> {
    let id = path.strip_prefix("/api/projects/")?.strip_suffix(leaf)?.strip_suffix('/')?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

/// The profiles a run can be started or switched with, for the web's pickers. It changes nothing, so it
/// is not audited, but it is there only while actions are on.
fn launch_profiles(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str) -> Response {
    action_read(roots, provider, "the launch profiles could not be read", |provider| provider.launch_profiles(id))
}

/// A read only the web's actions need: 404 unless actions are on or when the provider does not know it,
/// and a failure on this machine is only said to have failed, since its reason can name a path here.
fn action_read(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, failed: &str, read: impl FnOnce(&dyn ProjectsProvider) -> Result<Option<Value>, String>) -> Response {
    if !remote_actions_on(roots) {
        return error(404, "not found");
    }
    match provider.map(read) {
        Some(Ok(Some(found))) => reply(200, found),
        Some(Err(_)) => error(500, failed),
        _ => error(404, "not found"),
    }
}

/// A feature's editable fields and the ledger's version, for the web's editor. It changes nothing, so it is
/// not audited, but it is there only while actions are on.
fn feature_fields(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, feature: &str) -> Response {
    action_read(roots, provider, "the feature could not be read", |provider| provider.feature_fields(id, feature))
}

/// Where a run would continue if resumed, for the web's question before Resume. It changes nothing
/// the web asked for, so it is not audited, but it is there only while actions are on.
fn resume_check(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, run: &str) -> Response {
    if !remote_actions_on(roots) || !plain_run_id(run) {
        return error(404, "not found");
    }
    match provider.map(|provider| provider.resume_check(id, run)) {
        Some(Ok(RunControlOutcome::Done(next))) => reply(200, json!({ "run": run, "next": next })),
        Some(Ok(RunControlOutcome::Refused(why))) => reply(409, json!({ "error": why })),
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(_)) => error(500, "the run could not be checked"),
        _ => error(404, "not found"),
    }
}

#[allow(clippy::too_many_arguments)]
fn route(roots: &StorageRoots, config: &RemoteConfig, project_id: &str, project_name: &str, request: &Request, body: &[u8], provider: Option<&dyn ProjectsProvider>, confirm: Option<StepUp>) -> Response {
    // A stranger's address and a bad token are refused identically, so a
    // caller learns nothing about which check failed.
    if !peer_allowed(request.peer) || !token_matches(&config.token, request.authorization) {
        return error(401, "a valid bearer token is required");
    }
    let (path, query) = request.path.split_once('?').unwrap_or((request.path, ""));
    if let Some((id, feature)) = run_start_target(path) {
        match request.method {
            "POST" => return start_run(roots, provider, id, feature, body),
            "GET" | "HEAD" => {}
            _ => return error(405, "this view is read-only"),
        }
    }
    if let Some((id, run, control)) = (request.method == "POST").then(|| run_control_target(path)).flatten() {
        return control_run(roots, provider, id, run, control, body);
    }
    if let Some((id, run)) = (request.method == "POST").then(|| run_target(path, "discard")).flatten() {
        return discard_run(roots, provider, id, run, body);
    }
    if let Some((id, run)) = (request.method == "POST").then(|| run_target(path, "model")).flatten() {
        return switch_model(roots, provider, id, run, body);
    }
    if let Some((id, feature)) = (request.method == "POST").then(|| feature_target(path, "promote")).flatten() {
        return promote_feature(roots, provider, id, feature, body);
    }
    if let Some(id) = (request.method == "POST").then(|| project_target(path, "features")).flatten() {
        return save_feature(roots, provider, id, None, body);
    }
    if let Some((id, feature)) = (request.method == "POST").then(|| feature_path(path)).flatten() {
        return save_feature(roots, provider, id, Some(feature), body);
    }
    // Not behind the remote_actions gate: turning actions off must always be possible.
    if path == "/api/settings/remote-actions" && request.method == "POST" {
        return set_remote_actions(roots, body, confirm);
    }
    if path == "/api/settings/agents" && request.method == "POST" {
        return set_agents(roots, provider, body);
    }
    if !matches!(request.method, "GET" | "HEAD") {
        return error(405, "this view is read-only");
    }
    if let Some((id, run)) = run_target(path, "resume-check") {
        return resume_check(roots, provider, id, run);
    }
    if let Some((id, run)) = run_target(path, "discard-warning") {
        return discard_warning(roots, provider, id, run);
    }
    if let Some(id) = project_target(path, "launch-profiles") {
        return launch_profiles(roots, provider, id);
    }
    if let Some((id, feature)) = feature_target(path, "fields") {
        return feature_fields(roots, provider, id, feature);
    }
    if path == "/api/projects" {
        return match provider {
            Some(provider) => match provider.projects() {
                Ok(projects) => reply(200, json!({ "projects": projects })),
                Err(why) => error(500, &why),
            },
            None => error(404, "not found"),
        };
    }
    if path == "/api/audit" {
        return match crate::remote_audit::read(roots, AUDIT_LIMIT) {
            Ok(lines) => reply(200, json!({ "audit": lines })),
            Err(why) => error(500, &why),
        };
    }
    if path == "/api/settings/options" {
        return match provider.map(|provider| provider.setting_options()) {
            Some(Ok(Some(options))) => reply(200, json!({ "options": options })),
            Some(Err(_)) => error(500, "the options could not be read"),
            _ => error(404, "not found"),
        };
    }
    if path == "/api/settings" {
        return match provider.map(|provider| provider.settings()) {
            Some(Ok(Some(settings))) => reply(200, json!({ "settings": settings })),
            Some(Err(why)) => error(500, &why),
            _ => error(404, "not found"),
        };
    }
    if let Some(rest) = path.strip_prefix("/api/projects/") {
        if let Some(id) = rest.strip_suffix("/features").filter(|id| !id.is_empty() && !id.contains('/')) {
            return match provider.map(|provider| provider.features(id)) {
                Some(Ok(Some(features))) => reply(200, json!({ "features": features, "remote_actions": remote_actions_on(roots) })),
                // The reason can name a path on this machine, so the caller is only told it failed.
                Some(Err(_)) => error(500, "the feature ledger could not be read"),
                _ => error(404, "not found"),
            };
        }
        if let Some(id) = rest.strip_suffix("/runs").filter(|id| !id.is_empty() && !id.contains('/')) {
            return match provider.map(|provider| provider.runs(id)) {
                Some(Ok(Some(runs))) => match ListQuery::parse(query).and_then(|q| q.apply(runs, "run_id", "state")) {
                    Ok(page) => reply(200, json!({ "runs": page.rows, "next_cursor": page.next_cursor, "total": page.total, "running": page.running, "remote_actions": remote_actions_on(roots) })),
                    Err((status, why)) => error(status, why),
                },
                Some(Err(_)) => error(500, "the runs could not be read"),
                _ => error(404, "not found"),
            };
        }
        let parts: Vec<&str> = rest.split('/').collect();
        let logs = match parts.as_slice() {
            [id, "runs", run, "logs"] => Some((*id, *run, None, None)),
            [id, "runs", run, "logs", file] => Some((*id, *run, Some(*file), None)),
            [id, "runs", run, "logs", file, "after", cursor] => Some((*id, *run, Some(*file), Some(*cursor))),
            _ => None,
        };
        if let Some((id, run, file, after)) = logs.filter(|(id, run, file, after)| !id.is_empty() && !run.is_empty() && file.map_or(true, |file| !file.is_empty()) && after.map_or(true, |cursor| !cursor.is_empty())) {
            return match provider.map(|provider| provider.run_logs(id, run, file, after)) {
                Some(Ok(Some(logs))) => reply(200, logs),
                Some(Err(_)) => error(500, "the logs could not be read"),
                _ => error(404, "not found"),
            };
        }
        let timeline = match parts.as_slice() {
            [id, "runs", run] => Some((*id, *run, None)),
            [id, "runs", run, "after", cursor] => Some((*id, *run, Some(*cursor))),
            _ => None,
        };
        if let Some((id, run, after)) = timeline.filter(|(id, run, after)| !id.is_empty() && !run.is_empty() && after.map_or(true, |cursor| !cursor.is_empty())) {
            return match provider.map(|provider| provider.run_timeline(id, run, after)) {
                // The run detail offers the run's actions only when they are on.
                Some(Ok(Some(mut timeline))) => {
                    timeline["remote_actions"] = json!(remote_actions_on(roots));
                    reply(200, timeline)
                }
                Some(Err(_)) => error(500, "the run could not be read"),
                _ => error(404, "not found"),
            };
        }
        return match (provider, project_path(rest)) {
            (Some(provider), Some((id, run))) => match provider.review_scope(id) {
                Ok(Some(scope)) => match run {
                    None => list(roots, &scope.review_key, &scope.name, true, query),
                    Some(run_id) => one(roots, run_id, Some(&scope.review_key)),
                },
                Ok(None) => error(404, "not found"),
                Err(why) => error(500, &why),
            },
            _ => error(404, "not found"),
        };
    }
    if path == "/api/reviews" {
        return list(roots, project_id, project_name, false, query);
    }
    match path.strip_prefix("/api/reviews/") {
        Some(run_id) if !run_id.contains('/') => one(roots, run_id, None),
        _ => error(404, "not found"),
    }
}

/// The filter, sort and paging a list endpoint accepts in its query string.
struct ListQuery {
    state: Option<String>,
    oldest: bool,
    limit: Option<usize>,
    cursor: Option<String>,
}

/// One query component with `%XX` escapes and `+` decoded; `None` when an escape is broken or the bytes are not text.
fn query_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' => {
                let hex = raw.get(at + 1..at + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok().filter(|_| hex.bytes().all(|b| b.is_ascii_hexdigit()))?);
                at += 3;
            }
            b'+' => {
                out.push(b' ');
                at += 1;
            }
            byte => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

struct Page {
    rows: Vec<Value>,
    next_cursor: Option<String>,
    /// Rows left after the filter, before paging.
    total: usize,
    /// Rows in the `running` state in the whole set, whatever the filter and the page.
    running: usize,
}

/// Most audit lines one look returns, newest first.
const AUDIT_LIMIT: usize = 200;
const MAX_PAGE: usize = 200;

impl ListQuery {
    fn parse(query: &str) -> Result<Self, (u16, &'static str)> {
        let mut parsed = ListQuery { state: None, oldest: false, limit: None, cursor: None };
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let (Some(key), Some(value)) = (query_decode(key), query_decode(value)) else { return Err((400, "the query is not valid")) };
            match key.as_str() {
                "state" | "status" => parsed.state = Some(value),
                "cursor" => parsed.cursor = Some(value),
                "sort" => match value.as_str() {
                    "newest" => parsed.oldest = false,
                    "oldest" => parsed.oldest = true,
                    _ => return Err((400, "sort must be newest or oldest")),
                },
                "limit" => match value.parse::<usize>() {
                    Ok(limit) if (1..=MAX_PAGE).contains(&limit) => parsed.limit = Some(limit),
                    _ => return Err((400, "limit must be a number from 1 to 200")),
                },
                _ => return Err((400, "unknown query parameter")),
            }
        }
        Ok(parsed)
    }

    /// `rows` come newest first; the page is cut after the row whose `id_key` is the cursor.
    fn apply(&self, rows: Value, id_key: &str, state_key: &str) -> Result<Page, (u16, &'static str)> {
        let Value::Array(mut rows) = rows else { return Err((500, "the list could not be read")) };
        let running = rows.iter().filter(|row| row[state_key] == "running").count();
        if let Some(state) = &self.state {
            rows.retain(|row| row[state_key] == state.as_str());
        }
        if self.oldest {
            rows.reverse();
        }
        let total = rows.len();
        let start = match &self.cursor {
            None => 0,
            Some(cursor) => rows.iter().position(|row| row[id_key] == cursor.as_str()).ok_or((404, "not found"))? + 1,
        };
        let mut rows = rows.split_off(start);
        let mut next_cursor = None;
        if let Some(limit) = self.limit.filter(|limit| *limit < rows.len()) {
            rows.truncate(limit);
            next_cursor = rows.last().and_then(|row| row[id_key].as_str()).map(String::from);
        }
        Ok(Page { rows, next_cursor, total, running })
    }
}

/// `{id}/reviews` gives (id, None); `{id}/reviews/{run}` gives (id, Some(run)).
fn project_path(rest: &str) -> Option<(&str, Option<&str>)> {
    let (id, tail) = rest.split_once('/')?;
    if id.is_empty() {
        return None;
    }
    match tail.strip_prefix("reviews")? {
        "" => Some((id, None)),
        run => run.strip_prefix('/').filter(|run| !run.contains('/')).map(|run| (id, Some(run))),
    }
}

/// The newest stored lifecycle of `run_id` for the commit its report is about.
struct Facts {
    project: String,
    head: String,
    observed: Option<String>,
    link: Option<String>,
    stale: bool,
    /// `current`, `stale` or `unverified`, decided here and nowhere else.
    freshness: &'static str,
    /// The head the provider showed: the reviewed commit when current, the other one when stale.
    provider_head: Option<String>,
    checked_at: Option<String>,
}

/// Seconds since the epoch as plain UTC text, `2026-11-09T10:00:00Z`.
fn utc_text(seconds: i64) -> String {
    let (days, rem) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn facts(roots: &StorageRoots, run_id: &str) -> Option<Facts> {
    read_facts(roots, run_id).ok().flatten()
}

/// One read of the newest report record fixes the project, the commit and the lifecycle facts together.
/// A missing index or run is `None`; an index that cannot be read is an error, never "not found".
fn read_facts(roots: &StorageRoots, run_id: &str) -> Result<Option<Facts>, String> {
    // Only a genuinely absent index is "none"; one that cannot be looked at is an error.
    if !roots.database().try_exists().map_err(|err| format!("cannot look for the review index: {err}"))? {
        return Ok(None);
    }
    let store = Store::open_read_only(&roots.database()).map_err(|err| format!("cannot open the review index: {err}"))?;
    let records = store.reviews_for_run(run_id).map_err(|err| format!("cannot read the review index: {err}"))?;
    let Some(record) = records.into_iter().last() else { return Ok(None) };
    let (project, head) = (record.project, record.head_sha);
    // A lifecycle that cannot be read is unknown, never "fresh": it counts as stale and offers no link.
    let unknown = |project, head| Facts { project, head, observed: None, link: None, stale: true, freshness: "unverified", provider_head: None, checked_at: None };
    let Ok(all) = store.lifecycles_for_run(run_id) else { return Ok(Some(unknown(project, head))) };
    let Some(lifecycle) = all.into_iter().rev().find(|l| l.reported_head_sha == head) else {
        // Never looked at the provider: not current, but not known to be out of date either.
        return Ok(Some(Facts { stale: false, ..unknown(project, head) }));
    };
    // A look whose time cannot be read is not evidence either.
    let checked = store.lifecycle_checked_at(run_id, &head);
    let checked_at = checked.as_ref().ok().copied().flatten().map(utc_text);
    // The newest look decides: a mismatch stays stale whatever came after; a failed look is unverified.
    let freshness = if lifecycle.stale { "stale" } else if lifecycle.last_error.is_some() || !lifecycle.observed || checked_at.is_none() { "unverified" } else { "current" };
    let link = safe_link(&lifecycle.url).filter(|_| freshness == "current");
    let provider_head = match freshness {
        "current" => Some(head.clone()),
        "stale" => lifecycle.observed_head_sha.clone(),
        _ => None,
    };
    Ok(Some(Facts { observed: lifecycle.observed_head_sha, link, stale: lifecycle.stale, freshness, provider_head, checked_at, project, head }))
}

fn list(roots: &StorageRoots, project_id: &str, project_name: &str, only_this_project: bool, query: &str) -> Response {
    let query = match ListQuery::parse(query) {
        Ok(query) => query,
        Err((status, why)) => return error(status, why),
    };
    let rows = if only_this_project { review_inbox::inbox_of_project(roots, project_id, project_name) } else { review_inbox::inbox(roots, project_id, project_name) };
    let rows = match rows {
        Ok(rows) => rows,
        Err(why) => return error(500, &why),
    };
    let reviews: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            let f = facts(roots, &row.run_id);
            let freshness = f.as_ref().map_or("unverified", |f| f.freshness);
            let link = f.and_then(|f| f.link);
            let link_allowed = link.is_some();
            json!({
                "generated_at": row.generated_at,
                "run_id": row.run_id, "feature": row.feature, "project": row.project, "current": row.current,
                "status": row.status, "risk": row.risk, "stale": row.stale, "link": link,
                "freshness": freshness, "link_allowed": link_allowed,
            })
        })
        .collect();
    match query.apply(Value::Array(reviews), "run_id", "status") {
        Ok(page) => reply(200, json!({ "reviews": page.rows, "next_cursor": page.next_cursor, "total": page.total })),
        Err((status, why)) => error(status, why),
    }
}

fn one(roots: &StorageRoots, run_id: &str, only_project: Option<&str>) -> Response {
    if check_run_id(run_id).is_err() {
        return error(400, "invalid run id");
    }
    // The project and the commit come from the same record, so the check and the report cannot disagree.
    let f = match read_facts(roots, run_id) {
        Ok(Some(f)) if only_project.is_none_or(|owner| f.project == owner) => f,
        Ok(_) => return error(404, "no review report for this run"),
        Err(why) if only_project.is_some() => return error(500, &why),
        Err(_) => return error(404, "no review report for this run"),
    };
    let InboxDetail { summary, risks, files, tests, rounds, generated_at } = match review_inbox::detail_at(roots, run_id, &f.head) {
        Ok(detail) => detail,
        Err(_) => return error(404, "no review report for this run"),
    };
    reply(
        200,
        json!({
            "generated_at": generated_at,
            "run_id": run_id, "reported_head_sha": f.head, "observed_head_sha": f.observed, "stale": f.stale, "link": f.link,
            "freshness": f.freshness, "link_allowed": f.link.is_some(), "reviewed_head_sha": f.head, "provider_head_sha": f.provider_head, "checked_at": f.checked_at,
            "summary": summary, "risks": risks, "files": files, "tests": tests, "rounds": rounds,
        }),
    )
}
