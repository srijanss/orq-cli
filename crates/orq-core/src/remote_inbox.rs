//! A read-only view of the review inbox for a remote browser.
//!
//! This is the request handler only: it takes an already-parsed request and
//! returns a JSON answer, reading the same stored reports Herdr shows. It has
//! no way to change anything; merging happens at the provider, after
//! following the link in the answer. Every request is read from storage afresh,
//! so a lifecycle change shows on the next request.

use std::net::{IpAddr, Ipv6Addr};

use serde_json::{json, Value};

use crate::review_inbox::{self, ActionOutcome, InboxDetail, ReviewAction};
use crate::review_lifecycle::{ProviderState, LOCAL_PROVIDER};
use crate::storage::{check_run_id, StorageRoots};
use crate::store::todo::TodoChange;
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

    /// A new random token, written over the token file for its owner only. The new file is written whole
    /// and renamed into place, so a reader sees the old token or the new one, never part of either. The
    /// caller keeps the returned token; the file is not read back.
    pub fn rotate(roots: &StorageRoots) -> Result<RemoteConfig, String> {
        use std::io::{Read, Write};
        use std::os::unix::fs::OpenOptionsExt;
        let path = Self::token_path(roots);
        let mut bytes = [0u8; 40];
        std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).map_err(|e| format!("cannot generate a remote token: {}", e.kind()))?;
        let token: String = bytes[..32].iter().map(|b| format!("{b:02x}")).collect();
        let temp = path.with_file_name(format!(".remote-token.{}.tmp", bytes[32..].iter().map(|b| format!("{b:02x}")).collect::<String>()));
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temp)
            .and_then(|mut file| file.write_all(token.as_bytes()).and_then(|()| file.sync_all()))
            .and_then(|()| std::fs::rename(&temp, &path));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&temp);
            return Err(format!("cannot write the remote token file: {}", e.kind()));
        }
        Ok(RemoteConfig { token })
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

    /// What a push about `event` (of `run`, or of project `id` itself) shows: `{title, body, path}`. `None` when
    /// the project, the run or the event is not one this server shows.
    fn push_text(&self, _id: &str, _run: Option<&str>, _event: &str) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// The review scope of project `id`, `None` when there is no such project.
    fn review_scope(&self, _id: &str) -> Result<Option<ReviewScope>, String> {
        Ok(None)
    }

    /// The ledger features of project `id` as a JSON array, `None` when there is no such project.
    fn features(&self, _id: &str) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// The features archived out of project `id`'s ledger as `{"archived": [entry...]}`, `None` when there
    /// is no such project.
    fn archived_features(&self, _id: &str) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// Archives project `id`'s old completed features (a dry run only counts them); the reply is the
    /// counts as JSON.
    fn archive_features(&self, _id: &str, _dry_run: bool) -> Result<ArchiveOutcome, String> {
        Ok(ArchiveOutcome::Unknown)
    }

    /// Why project `id`'s settings refuse a run start now (its run limit or disk floor), `None` when
    /// a start may go or there is no such project.
    fn start_blocked(&self, _id: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    /// This machine's run limits as the dashboard shows them (its active runs, `[runs]` limits and
    /// free disk), `None` when no machine limit is set.
    fn machine_runs(&self) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// The orchestration runs of project `id`, newest first, as a JSON array; `None` when there is no such project.
    fn runs(&self, _id: &str) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// How the runs went over the last `days` days, for project `id` or every project when `None`, as
    /// the Stats page shows it; `None` when there is no such project.
    fn stats(&self, _id: Option<&str>, _days: u64) -> Result<Option<Value>, String> {
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

    /// One design image of a run of project `id` as `{content_type, data}` (data in base64): `path` is
    /// relative to the run's design folder and must be one its own design records list. `None` when the
    /// project, the run (of that project) or such an image is unknown.
    fn run_design_image(&self, _id: &str, _run: &str, _path: &str) -> Result<Option<Value>, String> {
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

    /// Starts one group run of `features` in project `id`, in that order, with the chosen profiles or the
    /// launch defaults. A provider that cannot start a group run says so.
    fn start_group_run(&self, _id: &str, _features: &[String], _profiles: Option<&LaunchProfiles>) -> Result<RunStart, String> {
        Ok(RunStart::Refused("this server cannot start a group run".to_string()))
    }

    /// Records on run `run` (whose reviews belong to the review key `project`) that a person resolved,
    /// dismissed or reopened its review of `head` from the web. The review itself is already changed.
    fn review_acted(&self, _project: &str, _run: &str, _action: ReviewAction, _head: &str) -> Result<(), String> {
        Ok(())
    }

    /// Pauses, resumes, stops or accepts run `run` of project `id`. An accept is done once its tests have
    /// started; they finish on their own. A provider that cannot does not know the run.
    fn control_run(&self, _id: &str, _run: &str, _control: RunControl) -> Result<RunControlOutcome, String> {
        Ok(RunControlOutcome::Unknown)
    }

    /// Resumes run `run` of project `id` as `control_run`'s Resume does, with the person's `answer`
    /// put in the resumed session's prompt. A provider that cannot says so.
    fn resume_with_answer(&self, _id: &str, _run: &str, _answer: &str) -> Result<RunControlOutcome, String> {
        Ok(RunControlOutcome::Refused("this server cannot resume a run with an answer".to_string()))
    }

    /// Resumes run `run` of project `id`, a frontend run that ended needing review or failed in its
    /// design review step, with a person's design `issues` (the JSON array `orq run resume
    /// --design-issues` reads) for one extra correction round. A provider that cannot says so.
    fn resume_with_design_issues(&self, _id: &str, _run: &str, _issues: &str) -> Result<RunControlOutcome, String> {
        Ok(RunControlOutcome::Refused("this server cannot take design issues".to_string()))
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

    /// Whether the draft `feature` of project `id` is to be queued once it is approved. A wish whose feature is no
    /// longer in the ledger (renamed or deleted) is dropped and reads as none. `None` when there is no such project.
    fn queue_wish(&self, _id: &str, _feature: &str) -> Result<Option<bool>, String> {
        Ok(None)
    }

    /// Asks (`wanted`) or no longer asks for the draft `feature` of project `id` to be queued once it is approved.
    /// `Some(Err(why))` when it is refused (only a draft in the ledger can be asked for); `None` when there is no
    /// such project.
    fn set_queue_wish(&self, _id: &str, _feature: &str, _wanted: bool) -> Result<Option<Result<(), String>>, String> {
        Ok(None)
    }

    /// Sets the kind of `feature` of project `id`, kept in orq.db, never in the ledger. `Some(Err(why))` when it is
    /// refused (only a feature in the ledger has a kind); `None` when there is no such project.
    fn set_feature_kind(&self, _id: &str, _feature: &str, _kind: crate::store::FeatureKind) -> Result<Option<Result<(), String>>, String> {
        Ok(None)
    }

    /// Project `id`'s run queue as the web shows it: `{entries: [{feature, state, run, added_at,
    /// added_by, implementer, reviewer}], paused}`; `None` when there is no such project.
    fn queue(&self, _id: &str) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// Changes project `id`'s run queue (`Done` with what changed), or says why not. A provider that
    /// cannot does not know the project.
    fn change_queue(&self, _id: &str, _change: &QueueChange) -> Result<RunControlOutcome, String> {
        Ok(RunControlOutcome::Unknown)
    }

    /// Pulls project `id`'s target from its remote, fast-forward only, as a person asked from the web:
    /// what came of it (`outcome`, `message`, and whether the warning it had is gone). `None` when there
    /// is no such project, as a provider that cannot pull says.
    fn pull_project_target(&self, _id: &str) -> Result<Option<Value>, String> {
        Ok(None)
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

    /// Project `id`'s own settings for its Settings page: each role as `{project, fallback}`, the
    /// delivery and the revision a save must name; `None` when there is no such project.
    fn project_settings(&self, _id: &str) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// Project `id`'s stored design preparer, design reviewer and design verifier, unredacted, which a
    /// save that leaves one out keeps; `None` when there is no such project or nothing is stored.
    fn design_agents(&self, _id: &str) -> Result<Option<[Option<crate::project_settings::ProjectAgent>; 3]>, String> {
        Ok(None)
    }

    /// What `pi --list-models` prints on this machine, for checking which models a design role may use;
    /// `None` when pi cannot be asked.
    fn pi_models(&self) -> Result<Option<String>, String> {
        Ok(None)
    }

    /// Saves `settings` as project `id`'s own, if its record is still at `revision`, and gives the
    /// settings read back. A provider that cannot does not know the project.
    fn save_project_settings(&self, _id: &str, _revision: u64, _settings: &crate::project_settings::ProjectSettings) -> Result<ProjectSettingsSave, String> {
        Ok(ProjectSettingsSave::Unknown)
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

    /// Cleans up run `run` of project `id` after its MR merged: marks its feature completed, deletes its
    /// worktree and branch and fast-forwards the main checkout's target, in order, each only when it is
    /// safe, stopping at the first that is not. A provider that cannot does not know the run.
    fn clean_up_run(&self, _id: &str, _run: &str) -> Result<RunCleanup, String> {
        Ok(RunCleanup::Unknown)
    }

    /// Whether project `id`'s target may be pushed to origin now, why not, and the commits a push
    /// would send, as `{provider, remote, target, sha, origin_sha, count, commits, more, reason}`;
    /// `None` when there is no such project.
    fn push_target_state(&self, _id: &str) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// Pushes project `id`'s target to origin at `sha`, the commit the person confirmed, once every
    /// check passes again. A provider that cannot does not know the project.
    fn push_target(&self, _id: &str, _sha: &str) -> Result<TargetPushResult, String> {
        Ok(TargetPushResult::Unknown)
    }

    /// What a storage cleanup would remove now, as `{lines, files, bytes, size, runs, metadata, hash}`; the
    /// hash names that set. `None` when this provider cannot clean up.
    fn storage_preview(&self) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// Removes the cleanup set if it still has `hash`. `None` when this provider cannot clean up.
    fn storage_prune(&self, _hash: &str) -> Result<Option<StoragePrune>, String> {
        Ok(None)
    }

    /// The worktrees orq made and kept, across every project, as `{projects: [{id, name, worktrees}], count,
    /// bytes, size}`. `None` when this provider has none to show.
    fn kept_worktrees(&self) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// Deletes project `project`'s kept worktree on `branch` and its branch, if it still has `version`; one
    /// with commits or files that would be lost needs `force`. A provider that cannot does not know it.
    fn delete_kept_worktree(&self, _project: &str, _branch: &str, _version: &str, _force: bool) -> Result<WorktreeDelete, String> {
        Ok(WorktreeDelete::Unknown)
    }

    /// Deletes each `(project, branch, version)` in turn as `delete_kept_worktree` would, answering for each
    /// in order. A provider can look its projects and runs up once for the whole batch, as long as it
    /// still checks each worktree's active runs just before deleting it.
    fn delete_kept_worktrees(&self, items: &[(&str, &str, &str)], force: bool) -> Vec<Result<WorktreeDelete, String>> {
        items.iter().map(|(project, branch, version)| self.delete_kept_worktree(project, branch, version, force)).collect()
    }

    /// The server answering, when it can show its build and restart itself; `None` otherwise.
    fn server(&self) -> Option<&dyn ServerControl> {
        None
    }

    /// Starts Draft with orq in project `id`: a planning run that drafts `request`'s idea. A provider that
    /// cannot does not know the project.
    fn start_plan(&self, _id: &str, _request: &PlanRequest) -> Result<PlanStart, String> {
        Ok(PlanStart::Unknown)
    }

    /// Planning run `run` of project `id` as the web shows it: `{state, source, feature, reason, proposal,
    /// current, decision}`. `None` when there is no such planning run.
    fn plan(&self, _id: &str, _run: &str) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// The person's choice on planning run `run`'s improved draft: Replace (`replace`) or Discard.
    fn decide_plan(&self, _id: &str, _run: &str, _replace: bool) -> Result<PlanDecision, String> {
        Ok(PlanDecision::Unknown)
    }

    /// Where a chat of project `id` runs and how its turns start; `None` when there is no such project or this
    /// provider cannot run chats.
    fn chat_place(&self, _id: &str) -> Result<Option<crate::ask_orq::ChatPlace>, String> {
        Ok(None)
    }
}

/// What Draft with orq drafts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanIdea {
    /// Todo item `id` as read at `version`, which becomes a draft feature and leaves the list.
    Todo { id: i64, version: i64, name: String, text: String },
    /// Draft feature `feature`, whose improved text waits for Replace or Discard.
    Draft { feature: String },
}

/// A Draft with orq request: the idea, the person's notes for the agent, and the harness and model that draft it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRequest {
    pub idea: PlanIdea,
    pub notes: String,
    pub harness: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanStart {
    Started(String),
    Refused(String),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanDecision {
    Done,
    Refused(String),
    Unknown,
}

/// The running server itself, as `orq serve` offers it to the web: its build, the installed orq's, and a
/// restart once no run is active.
pub trait ServerControl: Send + Sync {
    /// What Settings shows: `{running, installed, newer_installed, restart_pending, waiting_for}`.
    fn status(&self) -> Value;
    /// Asks for a restart once no run is active: how many runs it waits for, or why it may not restart
    /// (safe to show).
    fn restart_when_idle(&self) -> Result<usize, String>;
    /// Drops a pending restart; whether one was pending.
    fn cancel_restart(&self) -> bool;
    /// Whether a restart waits for the active runs to end.
    fn restart_pending(&self) -> bool;
}

/// How deleting a kept worktree from the web ended.
#[derive(Debug, Clone, PartialEq)]
pub enum WorktreeDelete {
    /// It was deleted; the value says what, and is safe to show.
    Deleted(Value),
    /// Why it was not deleted; safe to show.
    Refused(String),
    /// No kept worktree of a known project is on that branch.
    Unknown,
}

/// How a storage cleanup from the web ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoragePrune {
    /// The set was removed; the lines say what was freed. Safe to show.
    Pruned(Vec<String>),
    /// The set is not the one previewed; nothing was removed.
    Changed,
    /// Why the cleanup may not run now; safe to show.
    Refused(String),
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

/// How a save of a project's own settings ended.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectSettingsSave {
    /// Saved; the project's settings as a read now gives them.
    Saved(Value),
    /// The record was saved since the revision presented; nothing was written.
    Conflict,
    /// No such project.
    Unknown,
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
    /// Resume a run that needs review or failed to review the commits after its last review again,
    /// fix what that review finds and change nothing else.
    ReviewAgain,
    Stop,
    Accept,
    /// Merge a run blocked by uncommitted changes in the checkout, setting those changes aside meanwhile.
    MergeSetAside,
    /// Build the review report an ended run could not build itself.
    BuildReport,
}

/// How a Clean up of a merged run ended.
pub enum RunCleanup {
    /// The steps were run as far as they could go: the Clean up card as it now stands, `{steps}`.
    Done(Value),
    /// Why this run may not be cleaned up now; safe to show.
    Refused(String),
    /// There is no such project or run.
    Unknown,
}

/// How a push of a project's target ended.
pub enum TargetPushResult {
    /// Origin's `target` moved from `from` to `to` with `count` commits, carrying the commits of `runs`.
    Pushed { target: String, from: String, to: String, count: usize, runs: Vec<String> },
    /// Why nothing was pushed; safe to show.
    Refused(String),
    /// There is no such project.
    Unknown,
}

/// How a request to archive a project's completed features ended.
pub enum ArchiveOutcome {
    /// Archived (or counted, on a dry run): the counts as the reply.
    Done(Value),
    /// Left alone, in words fit to show (a run is active).
    Refused(String),
    /// No such project or ledger.
    Unknown,
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

/// A change to a project's run queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueueChange {
    /// Queue these features, each started with `profiles` when given, else the project's own.
    Add { features: Vec<String>, profiles: Option<LaunchProfiles> },
    /// Queue a Resume, Review again or Accept of an existing run of the project.
    AddAction(crate::store::run_queue::QueueAction),
    /// Remove the entry named so: a feature, or a run action's `<kind>:<run>`.
    Remove(String),
    /// Put the queue in this order, which names every entry once.
    Reorder(Vec<String>),
    Pause,
    Resume,
}

/// How a request to start a run ended.
pub enum RunStart {
    /// The run that began, by id.
    Started(String),
    /// The run that began, with what its starter should know about it, such as commits only the local
    /// target branch has, which the run will not see; safe to show.
    StartedWithWarning { run: String, warning: String },
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
    Ok(json!({"implementer": section("implementer"), "reviewer": section("reviewer"), "ask_orq_agent": section("ask_orq_agent"), "models": models, "remote_actions": remote_actions, "version": crate::remote_settings::version_of(&text)}))
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

/// A check that an assertion from the page confirms the change asked for (see [`step_up_purpose`]),
/// given only for a request from a passkey session; it spends the challenge the assertion answers.
pub type StepUp<'a> = &'a dyn Fn(&Value) -> bool;

/// The purpose a step-up assertion for a request on `path` must have been issued for: allowing a rule in
/// an Ask orq chat has its own, so neither challenge can be spent on the other change.
pub fn step_up_purpose(path: &str) -> &'static str {
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    match crate::ask_orq::target(path) {
        Some((_, tail)) if matches!(tail.as_slice(), [_, "allow"]) => "chat.allow",
        _ => "remote_actions.on",
    }
}

/// The risky action a `method` request on `path` with `body` is, as its audit action, when a passkey session
/// may take it only within a few minutes of a step-up; `None` for any other request.
///
/// A chat allow carrying its own purpose-bound `assertion` is let through here: that fresh assertion is checked
/// where the rule is allowed. Project settings count only when they turn the queue's hours (its auto-start) or
/// automatic after-merge cleanup on.
pub fn step_up_action(method: &str, path: &str, body: &[u8], provider: Option<&dyn ProjectsProvider>) -> Option<&'static str> {
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    if method != "POST" {
        return None;
    }
    let fields = || serde_json::from_slice::<Value>(body).ok().unwrap_or(Value::Null);
    if let Some((_, tail)) = crate::ask_orq::target(path) {
        return (matches!(tail.as_slice(), [_, "allow"]) && fields().get("assertion").is_none()).then_some("chat.allow");
    }
    if run_target(path, "discard").is_some() {
        return Some("run.discard");
    }
    if run_target(path, "cleanup").is_some() {
        return Some("run.cleanup");
    }
    if matches!(run_control_target(path), Some((_, _, RunControl::Accept))) {
        return Some("run.accept");
    }
    if path == "/api/storage/worktrees/delete-merged" {
        return Some("worktrees.delete_merged");
    }
    if path.starts_with("/api/storage/worktrees/") {
        return Some("worktree.delete");
    }
    if path == "/api/server/restart" {
        return Some("server.restart");
    }
    if let Some(id) = project_target(path, "settings") {
        let fields = fields();
        let current = || provider.and_then(|provider| provider.project_settings(id).ok().flatten()).unwrap_or(Value::Null);
        let hours = |value: &Value| value["queue_hours"].as_str().map(str::trim).filter(|hours| !hours.is_empty()).map(str::to_string);
        if hours(&fields).is_some_and(|wanted| hours(&current()) != Some(wanted)) {
            return Some("project.queue_auto_start");
        }
        if fields["after_merge"] == "auto" && current()["after_merge"] != "auto" {
            return Some("project.after_merge_auto");
        }
    }
    None
}

/// Like [`handle_with_body`], from a passkey session whose step-up assertions `confirm` checks.
#[allow(clippy::too_many_arguments)]
pub fn handle_with_confirm(roots: &StorageRoots, config: &RemoteConfig, project_id: &str, project_name: &str, request: &Request, body: &[u8], provider: &dyn ProjectsProvider, confirm: Option<StepUp>) -> Response {
    answer(roots, config, project_id, project_name, request, body, Some(provider), confirm)
}

/// The most bytes an action on `path` takes in its body.
pub fn action_body_cap(path: &str) -> usize {
    if path == "/api/storage/worktrees/delete-merged" {
        MAX_MERGED_BODY
    } else if path == "/api/todo" || path.starts_with("/api/todo/") {
        MAX_TODO_BODY
    } else if run_target(path, "design-issues").is_some() {
        MAX_DESIGN_ISSUES_BODY
    } else if crate::ask_orq::target(path).is_some() {
        crate::ask_orq::MAX_BODY
    } else {
        MAX_ACTION_BODY
    }
}

/// Why an action's body is refused, if it is: it must be empty or a JSON object with only `allowed` fields.
fn body_refusal(body: &[u8], allowed: &[&str]) -> Option<Response> {
    body_refusal_within(body, allowed, MAX_ACTION_BODY)
}

/// [`body_refusal`] for an action whose body may be up to `max` bytes.
fn body_refusal_within(body: &[u8], allowed: &[&str], max: usize) -> Option<Response> {
    if body.len() > max {
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

/// The project, run and control of `/api/projects/{id}/runs/{run}/pause`, `.../resume`, `.../stop`, `.../accept` or
/// `.../merge-set-aside` or `.../build-report`, when the path has that shape.
fn run_control_target(path: &str) -> Option<(&str, &str, RunControl)> {
    let mut parts = path.strip_prefix("/api/projects/")?.split('/');
    let (id, runs, run, action) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if runs != "runs" || id.is_empty() || run.is_empty() || parts.next().is_some() {
        return None;
    }
    match action {
        "pause" => Some((id, run, RunControl::Pause)),
        "resume" => Some((id, run, RunControl::Resume)),
        "review-again" => Some((id, run, RunControl::ReviewAgain)),
        "stop" => Some((id, run, RunControl::Stop)),
        "accept" => Some((id, run, RunControl::Accept)),
        "merge-set-aside" => Some((id, run, RunControl::MergeSetAside)),
        "build-report" => Some((id, run, RunControl::BuildReport)),
        _ => None,
    }
}

/// Whether the settings switch remote actions on; anything unreadable counts as off.
pub(crate) fn remote_actions_on(roots: &StorageRoots) -> bool {
    public_settings(&roots.config_file()).ok().and_then(|settings| settings["remote_actions"].as_bool()) == Some(true)
}

/// Leaves an audit line for an attempt on the server itself from the web.
fn server_audit(roots: &StorageRoots, action: &str, outcome: crate::remote_audit::Outcome, reason: Option<&str>) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let _ = crate::remote_audit::record(roots, &crate::remote_audit::Attempt { identity: "web", action, project: "server", run: None, outcome, reason }, now);
}

/// Asks the server to restart once no run is active, and leaves an audit line for the attempt.
/// It needs remote actions on and a passkey session (`passkey`): the token alone may not restart the server.
fn restart_server(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, body: &[u8], passkey: bool) -> Response {
    use crate::remote_audit::Outcome;
    let audit = |outcome, reason: Option<&str>| server_audit(roots, "server.restart", outcome, reason);
    if let Some(refusal) = body_refusal(body, &[]) {
        audit(Outcome::Refused, Some("bad request body"));
        return refusal;
    }
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, Some("remote actions are off"));
        return error(403, "remote actions are off");
    }
    if !passkey {
        audit(Outcome::Refused, Some("no passkey session"));
        return error(403, "restarting the server needs a passkey sign-in");
    }
    let Some(server) = provider.and_then(|provider| provider.server()) else { return error(404, "not found") };
    match server.restart_when_idle() {
        Ok(waiting_for) => {
            audit(Outcome::Allowed, None);
            reply(202, json!({ "restart_pending": true, "waiting_for": waiting_for }))
        }
        Err(why) => {
            audit(Outcome::Refused, Some(&why));
            error(409, &why)
        }
    }
}

/// Drops a pending restart, and leaves an audit line saying whether one was pending.
fn cancel_restart(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, body: &[u8]) -> Response {
    use crate::remote_audit::Outcome;
    let audit = |outcome, reason: Option<&str>| server_audit(roots, "server.restart_cancel", outcome, reason);
    if let Some(refusal) = body_refusal(body, &[]) {
        audit(Outcome::Refused, Some("bad request body"));
        return refusal;
    }
    let Some(server) = provider.and_then(|provider| provider.server()) else { return error(404, "not found") };
    let cancelled = server.cancel_restart();
    audit(Outcome::Allowed, (!cancelled).then_some("no restart was pending"));
    reply(200, json!({ "cancelled": cancelled }))
}

/// Turns remote actions off or on for the web, while the settings version presented is still current,
/// and leaves an audit line for the attempt. Turning them on needs a passkey session (`confirm`) and an
/// assertion in the body that it accepts.
/// The hours a passkey session may go unused that Settings may set, and the default.
pub const SESSION_IDLE_HOURS: (u64, u64, u64) = (1, 72, 12);
/// The days a passkey session may last in total that Settings may set, and the default.
pub const SESSION_MAX_DAYS: (u64, u64, u64) = (1, 30, 7);

/// The idle and absolute session limits in config.toml, as `(idle hours, max days)`; a value that is
/// missing, not a whole number or out of bounds is the default.
pub fn session_limit_settings(roots: &StorageRoots) -> (u64, u64) {
    let text = crate::remote_settings::read_config(&roots.config_file()).unwrap_or_default();
    let doc: Option<toml_edit::DocumentMut> = text.parse().ok();
    let read = |key: &str, (min, max, default): (u64, u64, u64)| {
        doc.as_ref().and_then(|doc| doc.get(key)).and_then(|item| item.as_integer()).and_then(|n| u64::try_from(n).ok()).filter(|n| (min..=max).contains(n)).unwrap_or(default)
    };
    (read("session_idle_hours", SESSION_IDLE_HOURS), read("session_max_days", SESSION_MAX_DAYS))
}

/// [`session_limit_settings`] in seconds, as the sessions take them.
pub fn session_limits(roots: &StorageRoots) -> (u64, u64) {
    let (hours, days) = session_limit_settings(roots);
    (hours * 60 * 60, days * 24 * 60 * 60)
}

/// GET /api/settings/sessions: the session limits and their bounds.
fn session_settings(roots: &StorageRoots) -> Response {
    let (idle_hours, max_days) = session_limit_settings(roots);
    reply(200, json!({ "idle_hours": idle_hours, "max_days": max_days, "idle_hours_bounds": [SESSION_IDLE_HOURS.0, SESSION_IDLE_HOURS.1], "max_days_bounds": [SESSION_MAX_DAYS.0, SESSION_MAX_DAYS.1] }))
}

/// POST /api/settings/sessions `{idle_hours, max_days}`: writes both limits, each within its bounds. Only a
/// passkey session may change how long sessions last. Audited as settings.sessions.
fn set_session_settings(roots: &StorageRoots, body: &[u8], passkey: bool) -> Response {
    use crate::remote_audit::Outcome;
    use crate::remote_settings::{read_config, version_of, write_if_version};
    let audit = |outcome, reason: Option<&str>| server_audit(roots, "settings.sessions", outcome, reason);
    if let Some(refusal) = body_refusal(body, &["idle_hours", "max_days"]) {
        audit(Outcome::Refused, Some("bad request body"));
        return refusal;
    }
    if !passkey {
        audit(Outcome::Refused, Some("needs a passkey sign-in"));
        return error(403, "Changing how long sessions last needs a passkey sign-in");
    }
    let fields = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
    let within = |key: &str, (min, max, _): (u64, u64, u64)| fields[key].as_u64().filter(|n| (min..=max).contains(n));
    let (Some(idle_hours), Some(max_days)) = (within("idle_hours", SESSION_IDLE_HOURS), within("max_days", SESSION_MAX_DAYS)) else {
        audit(Outcome::Refused, Some("out of bounds"));
        return error(422, &format!("Idle hours are {} to {} and days in total {} to {}", SESSION_IDLE_HOURS.0, SESSION_IDLE_HOURS.1, SESSION_MAX_DAYS.0, SESSION_MAX_DAYS.1));
    };
    let version = read_config(&roots.config_file()).map(|text| version_of(&text)).unwrap_or_default();
    let written = write_if_version(roots, &version, |doc| {
        doc["session_idle_hours"] = toml_edit::value(idle_hours as i64);
        doc["session_max_days"] = toml_edit::value(max_days as i64);
    });
    match written {
        Ok(()) => {
            audit(Outcome::Allowed, Some(&format!("idle {idle_hours} h, at most {max_days} days")));
            reply(200, json!({ "idle_hours": idle_hours, "max_days": max_days }))
        }
        Err(failed) => {
            audit(Outcome::Refused, Some(&failed.to_string()));
            error(failed.status(), &failed.to_string())
        }
    }
}

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
pub(crate) fn offered(options: &Value, harness: &str, model: &str) -> bool {
    options.as_array().is_some_and(|options| options.iter().any(|option| option["harness"] == harness && option["models"].as_array().is_some_and(|models| models.iter().any(|offered| offered == model))))
}

/// The effort levels `options` list for `harness`, or `None` when they list none for it.
fn efforts<'a>(options: &'a Value, harness: &str) -> Option<Vec<&'a str>> {
    let option = options.as_array()?.iter().find(|option| option["harness"] == harness)?;
    Some(option["efforts"].as_array()?.iter().filter_map(Value::as_str).collect())
}

/// What a save does to a role's effort: keep it (left out), clear it (null) or set it to a level.
#[derive(Clone, Copy)]
enum EffortChoice<'a> {
    Keep,
    Clear,
    Set(&'a str),
}

/// Sets the implementer's and reviewer's harness, model and effort for the web, if actions are on, from the
/// provider's options only and while the settings version presented is still current; a role left out
/// or null is not touched. An effort must be one of the levels listed for its harness, null clears it, and
/// one left out is kept unless the harness lists levels that do not include it. Leaves an audit line for
/// the attempt.
///
/// The Ask orq agent (`ask_orq_agent`) is set the same way, but only to a Claude model, since a chat relies on
/// Claude Code's settings and refusals, and only from a passkey session, since its replies can quote files.
fn set_agents(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, body: &[u8], confirm: Option<StepUp>) -> Response {
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
    if let Some(refusal) = body_refusal(body, &["implementer", "reviewer", "ask_orq_agent", "version"]) {
        audit(Outcome::Refused, Some("bad request body"));
        return refusal;
    }
    let fields = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
    let Some(version) = fields["version"].as_str() else { return bad_body() };
    let mut chosen: Vec<(&str, &str, &str, EffortChoice)> = Vec::new();
    for role in ["implementer", "reviewer", "ask_orq_agent"] {
        match &fields[role] {
            Value::Null => {}
            Value::Object(agent) if agent.keys().all(|key| ["harness", "model", "effort"].contains(&key.as_str())) => {
                let effort = match agent.get("effort") {
                    None => EffortChoice::Keep,
                    Some(Value::Null) => EffortChoice::Clear,
                    Some(Value::String(level)) => EffortChoice::Set(level),
                    Some(_) => return bad_body(),
                };
                match (agent.get("harness").and_then(Value::as_str), agent.get("model").and_then(Value::as_str)) {
                    (Some(harness), Some(model)) => chosen.push((role, harness, model, effort)),
                    _ => return bad_body(),
                }
            }
            _ => return bad_body(),
        }
    }
    if chosen.is_empty() {
        return bad_body();
    }
    if let Some((_, harness, _, _)) = chosen.iter().find(|(role, _, _, _)| *role == "ask_orq_agent") {
        if confirm.is_none() {
            audit(Outcome::Refused, Some("needs a passkey sign-in"));
            return error(403, "Changing the Ask orq agent needs a passkey sign-in");
        }
        if *harness != "claude" {
            audit(Outcome::Refused, Some("the Ask orq agent is Claude only"));
            return error(422, "The Ask orq agent can only be a Claude model");
        }
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
    if !chosen.iter().all(|(_, harness, model, _)| offered(&options, harness, model)) {
        audit(Outcome::Refused, Some("not an offered harness or model"));
        return error(422, "That harness or model is not one this machine offers");
    }
    let takes = |harness: &str, level: &str| efforts(&options, harness).is_some_and(|levels| levels.contains(&level));
    if !chosen.iter().all(|(_, harness, _, effort)| match effort {
        EffortChoice::Set(level) => takes(harness, level),
        _ => true,
    }) {
        audit(Outcome::Refused, Some("not an offered effort"));
        return error(422, "That effort is not one this harness takes");
    }
    // A role kept as something other than a table is left for a person to fix, and then nothing is
    // written, so a save never lands for one role and not the other.
    let mut not_a_table = false;
    let written = write_if_version(roots, version, |doc| {
        not_a_table = chosen.iter().any(|(role, _, _, _)| doc.get(role).is_some_and(|item| item.as_table_like().is_none()));
        if not_a_table {
            return;
        }
        for (role, harness, model, effort) in &chosen {
            let table = doc.entry(role).or_insert_with(toml_edit::table);
            if let Some(table) = table.as_table_like_mut() {
                table.insert("harness", toml_edit::value(*harness));
                table.remove("profile");
                table.insert("model", toml_edit::value(*model));
                match effort {
                    EffortChoice::Set(level) => {
                        table.insert("effort", toml_edit::value(*level));
                    }
                    EffortChoice::Clear => {
                        table.remove("effort");
                    }
                    // A kept effort the new harness lists no level for would only fail its runs.
                    EffortChoice::Keep => {
                        let kept = table.get("effort").and_then(|item| item.as_str()).map(str::to_string);
                        if let (Some(kept), Some(levels)) = (kept, efforts(&options, harness)) {
                            if !levels.contains(&kept.as_str()) {
                                table.remove("effort");
                            }
                        }
                    }
                }
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
    let (chosen, group) = match launch_choice(body, feature) {
        Ok(choice) => choice,
        Err(refusal) => {
            audit(Outcome::Refused, None, Some("bad request body"));
            return refusal;
        }
    };
    let started = provider.map(|provider| match (&group, &chosen) {
        (Some(group), profiles) => provider.start_group_run(id, group, profiles.as_ref()),
        (None, Some(profiles)) => provider.start_run_with(id, feature, profiles),
        (None, None) => provider.start_run(id, feature),
    });
    let grouped = group.as_ref().map(|group| format!("group {}", group.join(", ")));
    let profiled = chosen.as_ref().map(|profiles| format!("implementer {}, reviewer {}", profiles.implementer, profiles.reviewer));
    let named = match (grouped, profiled) {
        (Some(grouped), Some(profiled)) => Some(format!("{grouped}, {profiled}")),
        (grouped, profiled) => grouped.or(profiled),
    };
    match started {
        Some(Ok(RunStart::Started(run))) => {
            audit(Outcome::Allowed, Some(&run), named.as_deref());
            reply(202, json!({ "run": run }))
        }
        Some(Ok(RunStart::StartedWithWarning { run, warning })) => {
            audit(Outcome::Allowed, Some(&run), named.as_deref());
            reply(202, json!({ "run": run, "warning": warning }))
        }
        Some(Ok(RunStart::Refused(why))) => {
            audit(Outcome::Refused, None, Some(&why));
            reply(409, json!({ "error": why }))
        }
        // The reason can name a path on this machine, so the caller is only told it failed; the server's
        // own log, which only the operator reads, says why.
        Some(Err(why)) => {
            log_line(&launch_failure_line("run.start", id, feature, &why));
            audit(Outcome::Refused, None, Some("the run could not be started"));
            error(500, "the run could not be started")
        }
        _ => {
            audit(Outcome::Refused, None, Some("no such project or feature"));
            error(404, "not found")
        }
    }
}

/// The profiles and group a start body chooses. Profiles: none for an empty body or `{}`, which keep the launch
/// defaults, or both an implementer and a reviewer, each a non-empty name. Group: absent for a run of `feature`
/// alone, or the features of one group run in order, led by `feature`, each a distinct feature name; a group
/// of only `feature` is a run of it alone. Nothing else may be in the body.
fn launch_choice(body: &[u8], feature: &str) -> Result<(Option<LaunchProfiles>, Option<Vec<String>>), Response> {
    let bad = || error(400, "the request body is not one this action takes");
    if let Some(refusal) = body_refusal(body, &["implementer", "reviewer", "group"]) {
        return Err(refusal);
    }
    let Ok(Value::Object(mut fields)) = serde_json::from_slice::<Value>(body) else { return Ok((None, None)) };
    let group = match fields.remove("group") {
        None => None,
        Some(Value::Array(names)) => {
            let names: Vec<String> = names.iter().map(|name| name.as_str().filter(|name| !name.is_empty() && plain_feature_name(name)).map(str::to_string)).collect::<Option<_>>().ok_or_else(bad)?;
            let distinct = names.iter().enumerate().all(|(at, name)| !names[..at].contains(name));
            if names.first().map(String::as_str) != Some(feature) || !distinct {
                return Err(bad());
            }
            (names.len() > 1).then_some(names)
        }
        Some(_) => return Err(bad()),
    };
    if fields.is_empty() {
        return Ok((None, group));
    }
    let named = |key: &str| fields.get(key).and_then(Value::as_str).filter(|name| !name.trim().is_empty()).map(str::to_string);
    match (named("implementer"), named("reviewer")) {
        (Some(implementer), Some(reviewer)) => Ok((Some(LaunchProfiles { implementer, reviewer }), group)),
        _ => Err(bad()),
    }
}

/// The longest answer a queued Resume keeps, in characters.
pub const MAX_QUEUED_ANSWER: usize = crate::store::run_queue::MAX_ANSWER_CHARS;

/// Whether `name` names a queue entry: a feature, or a run action's `<kind>:<run>`.
fn queue_entry_name(name: &str) -> bool {
    match name.split_once(':') {
        Some((kind, run)) => crate::store::run_queue::QueueKind::parse(kind).is_some() && !run.is_empty() && plain_run_id(run),
        None => !name.is_empty() && plain_feature_name(name),
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
            if let Some(provider) = provider {
                queue_wished(roots, provider, id, feature, now);
            }
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

/// Sets a feature's kind (`{kind: "frontend"|"standard"}`) for the web's Frontend switch, if actions are on, and
/// leaves an audit line for the attempt as feature.kind, naming the feature.
fn set_feature_kind(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, feature: &str, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    use crate::store::FeatureKind;
    if !plain_feature_name(feature) {
        return error(404, "not found");
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let failed = "the kind could not be set";
    let audit = |outcome, reason: &str| {
        let _ = record(roots, &Attempt { identity: "web", action: "feature.kind", project: id, run: None, outcome, reason: Some(&format!("{feature}: {reason}")) }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, "remote actions are off");
        return error(404, "not found");
    }
    let kind = match body_object(body, MAX_ACTION_BODY) {
        Ok(sent) if sent.keys().all(|key| key == "kind") => sent.get("kind").and_then(Value::as_str).and_then(FeatureKind::parse),
        _ => None,
    };
    let Some(kind) = kind else {
        audit(Outcome::Refused, "bad request body");
        return error(400, "the kind is frontend or standard");
    };
    match provider.map(|provider| provider.set_feature_kind(id, feature, kind)) {
        Some(Ok(Some(Ok(())))) => {
            audit(Outcome::Allowed, kind.as_str());
            reply(200, json!({ "feature": feature, "kind": kind.as_str() }))
        }
        Some(Ok(Some(Err(why)))) => {
            audit(Outcome::Refused, &why);
            reply(409, json!({ "error": why }))
        }
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(_)) => {
            audit(Outcome::Refused, failed);
            error(500, failed)
        }
        _ => {
            audit(Outcome::Refused, "no such project");
            error(404, "not found")
        }
    }
}

/// Archives (or, with `dry_run`, counts) a project's old completed features for the web, if actions are on,
/// and leaves an audit line for the attempt as features.archive.
fn archive_features(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let failed = "the features could not be archived";
    let audit = |outcome, reason: &str| {
        let _ = record(roots, &Attempt { identity: "web", action: "features.archive", project: id, run: None, outcome, reason: Some(reason) }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, "remote actions are off");
        return error(404, "not found");
    }
    if let Some(refusal) = body_refusal(body, &["dry_run"]) {
        audit(Outcome::Refused, "bad request body");
        return refusal;
    }
    let dry_run = match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(fields)) => match fields.get("dry_run") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(flag)) => *flag,
            _ => {
                audit(Outcome::Refused, "bad request body");
                return error(400, "the request body is not one this action takes");
            }
        },
        _ => false,
    };
    match provider.map(|provider| provider.archive_features(id, dry_run)) {
        Some(Ok(ArchiveOutcome::Done(counts))) => {
            audit(Outcome::Allowed, if dry_run { "dry run" } else { "archived" });
            reply(200, counts)
        }
        Some(Ok(ArchiveOutcome::Refused(why))) => {
            audit(Outcome::Refused, &why);
            reply(409, json!({ "error": why }))
        }
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(_)) => {
            audit(Outcome::Refused, failed);
            error(500, failed)
        }
        _ => {
            audit(Outcome::Refused, "no such project");
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
    if let Some(refusal) = body_refusal(body, &["fields", "version"]) {
        return Err(refusal);
    }
    let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(body) else { return Err(error(400, "the request body is not one this action takes")) };
    feature_save_fields(&body, feature)
}

/// [`feature_save_body`]'s fields and version, from a body already read and checked for its size.
fn feature_save_fields(body: &serde_json::Map<String, Value>, feature: Option<&str>) -> Result<(FeatureInput, Option<String>), Response> {
    let bad = || error(400, "the request body is not one this action takes");
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

/// Queues the just-approved draft `feature` of project `id` when it was moved from Todo asking for that, spends
/// the wish, and leaves a queue.add audit line saying so.
fn queue_wished(roots: &StorageRoots, provider: &dyn ProjectsProvider, id: &str, feature: &str, now: u64) {
    use crate::remote_audit::{record, Attempt, Outcome};
    if provider.queue_wish(id, feature) != Ok(Some(true)) {
        return;
    }
    let _ = provider.set_queue_wish(id, feature, false);
    let why = format!("{feature}: requested when moved from Todo");
    let change = QueueChange::Add { features: vec![feature.to_string()], profiles: None };
    let (outcome, reason) = match provider.change_queue(id, &change) {
        Ok(RunControlOutcome::Done(_)) => (Outcome::Allowed, why),
        Ok(RunControlOutcome::Refused(refusal)) => (Outcome::Refused, format!("{why}: {refusal}")),
        _ => (Outcome::Refused, format!("{why}: the queue could not be changed")),
    };
    let _ = record(roots, &Attempt { identity: "web", action: "queue.add", project: id, run: None, outcome, reason: Some(&reason) }, now);
}

/// Whether the draft `feature` of project `id` is to be queued once approved (a read, there with actions off), or a
/// change of that wish (`{queue_when_approved}`), which needs actions on and leaves a queue.wish audit line.
fn queue_wish(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, feature: &str, method: &str, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    if !plain_feature_name(feature) {
        return error(404, "not found");
    }
    if method != "POST" {
        return match provider.map(|provider| provider.queue_wish(id, feature)) {
            Some(Ok(Some(wanted))) => reply(200, json!({ "queue_when_approved": wanted })),
            Some(Err(_)) => error(500, "the wish could not be read"),
            _ => error(404, "not found"),
        };
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let audit = |outcome, reason: &str| {
        let _ = record(roots, &Attempt { identity: "web", action: "queue.wish", project: id, run: None, outcome, reason: Some(&format!("{feature}: {reason}")) }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, "remote actions are off");
        return error(404, "not found");
    }
    let wanted = match body_object(body, MAX_ACTION_BODY) {
        Ok(sent) if sent.keys().all(|key| key == "queue_when_approved") => sent.get("queue_when_approved").and_then(Value::as_bool),
        _ => None,
    };
    let Some(wanted) = wanted else {
        audit(Outcome::Refused, "bad request body");
        return error(400, "the request body is not one this action takes");
    };
    match provider.map(|provider| provider.set_queue_wish(id, feature, wanted)) {
        Some(Ok(Some(Ok(())))) => {
            audit(Outcome::Allowed, if wanted { "requested" } else { "cancelled" });
            reply(200, json!({ "queue_when_approved": wanted }))
        }
        Some(Ok(Some(Err(why)))) => {
            audit(Outcome::Refused, &why);
            error(409, &why)
        }
        Some(Err(_)) => {
            audit(Outcome::Refused, "the wish could not be changed");
            error(500, "the wish could not be changed")
        }
        _ => {
            audit(Outcome::Refused, "no such project");
            error(404, "not found")
        }
    }
}

/// The most bytes a Todo change takes in its body: the longest description with every character escaped.
const MAX_TODO_BODY: usize = 256 * 1024;

/// What `/api/todo...` asks for: an add, or an edit, delete or move of item `id`.
#[derive(Clone, Copy)]
enum TodoTarget {
    Add,
    Edit(i64),
    Delete(i64),
    Move(i64),
    /// A path under `/api/todo/` that names no item.
    Unknown,
}

fn todo_target(path: &str) -> Option<TodoTarget> {
    if path == "/api/todo" {
        return Some(TodoTarget::Add);
    }
    let rest = path.strip_prefix("/api/todo/")?;
    let (id, leaf) = rest.split_once('/').unwrap_or((rest, ""));
    // Only an id written plainly names an item, so `/api/todo/+1` or `/api/todo/01` names none.
    let Some(number) = id.parse::<i64>().ok().filter(|number| *number > 0 && number.to_string() == id) else { return Some(TodoTarget::Unknown) };
    Some(match leaf {
        "" if !rest.ends_with('/') => TodoTarget::Edit(number),
        "delete" => TodoTarget::Delete(number),
        "move" => TodoTarget::Move(number),
        _ => TodoTarget::Unknown,
    })
}

fn todo_json(item: &crate::store::todo::TodoItem) -> Value {
    json!({
        "id": item.id,
        "name": item.name,
        "description": item.description,
        "created_at": item.created_at,
        "updated_at": item.updated_at,
        "version": item.version,
    })
}

/// The Todo list, newest first, and whether actions are on, so the page offers its changes only then.
/// No database yet reads as an empty list and creates nothing.
fn todo_list(roots: &StorageRoots) -> Response {
    let items = if roots.database().exists() { Store::open(&roots.database()).and_then(|store| store.todos()) } else { Ok(Vec::new()) };
    match items {
        Ok(items) => reply(200, json!({ "todos": items.iter().map(todo_json).collect::<Vec<_>>(), "remote_actions": remote_actions_on(roots) })),
        Err(_) => error(500, "the Todo list could not be read"),
    }
}

/// Adds, edits, deletes or moves a Todo item for the web, if actions are on, and leaves an audit line for the
/// attempt as todo.add, todo.edit, todo.delete or todo.move; a move's line names the project it went to.
fn change_todo(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, target: TodoTarget, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let action = match target {
        TodoTarget::Add => "todo.add",
        TodoTarget::Edit(_) | TodoTarget::Unknown => "todo.edit",
        TodoTarget::Delete(_) => "todo.delete",
        TodoTarget::Move(_) => "todo.move",
    };
    let sent = body_object(body, MAX_TODO_BODY);
    let project = match (target, &sent) {
        (TodoTarget::Move(_), Ok(sent)) => sent.get("project").and_then(Value::as_str).unwrap_or_default().to_string(),
        _ => String::new(),
    };
    let audit = |outcome, reason: &str| {
        let _ = record(roots, &Attempt { identity: "web", action, project: &project, run: None, outcome, reason: Some(reason) }, now);
    };
    let failed = "the Todo list could not be changed";
    let id = match target {
        TodoTarget::Add => None,
        TodoTarget::Edit(id) | TodoTarget::Delete(id) | TodoTarget::Move(id) => Some(id),
        TodoTarget::Unknown => {
            audit(Outcome::Refused, "no such item");
            return error(404, "not found");
        }
    };
    // Only an existing database holds items; an add creates it, but only with actions on. With them off the store
    // is opened read only, never migrated, to name the item in the refusal, and a store that cannot be read is no
    // reason to say otherwise.
    let actions = remote_actions_on(roots);
    let store = if (id.is_some() || !actions) && !roots.database().exists() {
        None
    } else {
        match if actions { Store::open(&roots.database()) } else { Store::open_read_only(&roots.database()) } {
            Ok(store) => Some(store),
            Err(_) if !actions => None,
            Err(_) => {
                audit(Outcome::Refused, failed);
                return error(500, failed);
            }
        }
    };
    let item = match (id, &store) {
        (Some(id), Some(store)) => match store.todo(id) {
            Ok(item) => item,
            Err(_) if !actions => None,
            Err(_) => {
                audit(Outcome::Refused, failed);
                return error(500, failed);
            }
        },
        _ => None,
    };
    let label = match (&item, &sent) {
        (Some(item), _) => Some(item.name.clone()),
        (None, Ok(sent)) if id.is_none() => sent.get("name").and_then(Value::as_str).map(str::to_string),
        _ => None,
    };
    let named = |why: &str| label.as_deref().map_or_else(|| why.to_string(), |label| format!("{label}: {why}"));
    if !actions {
        audit(Outcome::Refused, &named("remote actions are off"));
        return error(404, "not found");
    }
    let sent = match sent {
        Ok(sent) => sent,
        Err(refusal) => {
            audit(Outcome::Refused, &named("bad request body"));
            return refusal;
        }
    };
    let bad = |why: &str| {
        audit(Outcome::Refused, &named(why));
        error(400, "the request body is not one this action takes")
    };
    let gone = || {
        audit(Outcome::Refused, "no such item");
        error(404, "not found")
    };
    let broken = || {
        audit(Outcome::Refused, &named(failed));
        error(500, failed)
    };
    let Some(store) = store else { return gone() };
    let Some(id) = id else {
        let Some((name, description, _)) = todo_fields(&sent, false) else { return bad("bad request body") };
        return match store.todo_add(&name, &description, now as i64) {
            Ok(TodoChange::Done(item)) => {
                audit(Outcome::Allowed, &item.name);
                reply(200, json!({ "todo": todo_json(&item) }))
            }
            Ok(TodoChange::Invalid(why)) => {
                audit(Outcome::Refused, &named(&why));
                reply(400, json!({ "error": why }))
            }
            _ => broken(),
        };
    };
    let Some(item) = item else { return gone() };
    match target {
        TodoTarget::Edit(_) => {
            let Some((name, description, Some(version))) = todo_fields(&sent, true) else { return bad("bad request body") };
            match store.todo_edit(id, &name, &description, version, now as i64) {
                Ok(TodoChange::Done(item)) => {
                    audit(Outcome::Allowed, &item.name);
                    reply(200, json!({ "todo": todo_json(&item) }))
                }
                Ok(TodoChange::Invalid(why)) => {
                    audit(Outcome::Refused, &named(&why));
                    reply(400, json!({ "error": why }))
                }
                Ok(TodoChange::Conflict) => {
                    audit(Outcome::Refused, &named("the item changed since it was read"));
                    error(409, "The item changed since you opened it; nothing was written")
                }
                Ok(TodoChange::Unknown) => gone(),
                Err(_) => broken(),
            }
        }
        TodoTarget::Delete(_) => {
            if !sent.is_empty() {
                return bad("bad request body");
            }
            match store.todo_delete(id) {
                Ok(true) => {
                    audit(Outcome::Allowed, &item.name);
                    reply(200, json!({ "deleted": id }))
                }
                Ok(false) => gone(),
                Err(_) => broken(),
            }
        }
        TodoTarget::Move(_) => {
            const KEYS: [&str; 4] = ["project", "fields", "version", "queue_when_approved"];
            let queue_when_approved = match sent.get("queue_when_approved") {
                None | Some(Value::Null) => Some(false),
                Some(Value::Bool(wanted)) => Some(*wanted),
                _ => None,
            };
            // The version is the Todo item's as read, not the ledger's: a move adds a feature, which needs none.
            let read = sent.get("version").and_then(Value::as_i64);
            let (true, Some(queue_when_approved), Some(read)) = (sent.keys().all(|key| KEYS.contains(&key.as_str())) && !project.is_empty(), queue_when_approved, read) else {
                return bad("bad request body");
            };
            if read != item.version {
                audit(Outcome::Refused, &named("the item changed since it was read"));
                return error(409, "The item changed since you opened it; nothing was written");
            }
            let mut fields = sent.clone();
            fields.remove("version");
            let (input, version) = match feature_save_fields(&fields, None) {
                Ok(parsed) => parsed,
                Err(refusal) => {
                    audit(Outcome::Refused, &named("bad request body"));
                    return refusal;
                }
            };
            let feature = input.name.clone().unwrap_or_default();
            // The save is the editor's own; the item goes only once it succeeded, and only as it was read: an edit
            // made meanwhile keeps the item, and the caller is told the draft was saved from the older text.
            match provider.map(|provider| provider.save_feature(&project, None, &input, version.as_deref())) {
                Some(Ok(FeatureSave::Saved)) => match store.todo_delete_at(id, read) {
                    Ok(false) => {
                        audit(Outcome::Refused, &named("the draft was saved but the item changed meanwhile, so it was kept"));
                        error(409, "The draft was saved, but the Todo item changed meanwhile and was kept")
                    }
                    Ok(true) => {
                        audit(Outcome::Allowed, &named(&feature));
                        if let (true, Some(provider)) = (queue_when_approved, provider) {
                            let wished = matches!(provider.set_queue_wish(&project, &feature, true), Ok(Some(Ok(()))));
                            let (outcome, why) = if wished { (Outcome::Allowed, "requested") } else { (Outcome::Refused, "the wish could not be kept") };
                            let reason = format!("{feature}: {why}");
                            let _ = record(roots, &Attempt { identity: "web", action: "queue.wish", project: &project, run: None, outcome, reason: Some(&reason) }, now);
                        }
                        reply(200, json!({ "feature": feature, "project": project }))
                    }
                    Err(_) => {
                        audit(Outcome::Refused, &named("the draft was saved but the item could not be removed"));
                        error(500, "the draft was saved but the Todo item could not be removed")
                    }
                },
                Some(Ok(FeatureSave::Invalid(why))) => {
                    audit(Outcome::Refused, &named(&why));
                    reply(422, json!({ "error": why }))
                }
                Some(Ok(FeatureSave::Conflict(why))) => {
                    audit(Outcome::Refused, &named(&why));
                    reply(409, json!({ "error": why }))
                }
                // The reason can name a path on this machine, so the caller is only told it failed.
                Some(Err(_)) => {
                    audit(Outcome::Refused, &named("the feature could not be saved"));
                    error(500, "the feature could not be saved")
                }
                _ => {
                    audit(Outcome::Refused, &named("no such project"));
                    error(404, "not found")
                }
            }
        }
        TodoTarget::Add | TodoTarget::Unknown => gone(),
    }
}

/// The name, description (empty when not sent) and, for an edit, version of a Todo add or edit body.
fn todo_fields(sent: &serde_json::Map<String, Value>, edit: bool) -> Option<(String, String, Option<i64>)> {
    let keys: &[&str] = if edit { &["name", "description", "version"] } else { &["name", "description"] };
    if !sent.keys().all(|key| keys.contains(&key.as_str())) {
        return None;
    }
    let name = sent.get("name")?.as_str()?.to_string();
    let description = match sent.get("description") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        _ => return None,
    };
    let version = match sent.get("version") {
        None => None,
        Some(version) => Some(version.as_i64()?),
    };
    Some((name, description, version))
}

/// The last part of the paths that start Draft with orq.
const PLAN_LEAF: &str = "draft-with-claude";
/// The most characters of notes for the agent a request takes.
const MAX_PLAN_NOTES: usize = 8000;

/// The Todo item of `/api/todo/{id}/draft-with-claude`, when the path has that shape.
fn todo_plan_target(path: &str) -> Option<i64> {
    let id = path.strip_prefix("/api/todo/")?.strip_suffix(PLAN_LEAF)?.strip_suffix('/')?;
    id.parse::<i64>().ok().filter(|number| *number > 0 && number.to_string() == id)
}

/// The notes of a Draft with orq body: a string of at most MAX_PLAN_NOTES characters, empty when not sent.
fn plan_notes(sent: &serde_json::Map<String, Value>) -> Option<String> {
    match sent.get("notes") {
        None | Some(Value::Null) => Some(String::new()),
        Some(Value::String(notes)) if notes.chars().count() <= MAX_PLAN_NOTES => Some(notes.clone()),
        _ => None,
    }
}

/// The harness and model Draft with orq uses when the body leaves them out.
const PLAN_HARNESS: &str = "claude";
const PLAN_MODEL: &str = "haiku";

/// The harness and model of a Draft with orq body, `claude` and `haiku` when left out; `None` when one is not a string.
fn plan_agent(sent: &serde_json::Map<String, Value>) -> Option<(String, String)> {
    let field = |key: &str, default: &str| match sent.get(key) {
        None | Some(Value::Null) => Some(default.to_string()),
        Some(Value::String(text)) => Some(text.clone()),
        _ => None,
    };
    Some((field("harness", PLAN_HARNESS)?, field("model", PLAN_MODEL)?))
}

/// Why `harness` with `model` cannot draft on this machine: a harness orq cannot run read-only, or a harness and
/// model the provider's options do not offer. The refusal is the response and its audit reason.
fn plan_agent_refusal(provider: Option<&dyn ProjectsProvider>, harness: &str, model: &str) -> Option<(Response, &'static str)> {
    let not_offered = || Some((error(422, "That harness or model is not one this machine offers"), "not an offered harness or model"));
    if !crate::project_settings::DESIGN_HARNESSES.contains(&harness) {
        return not_offered();
    }
    match provider.map(|provider| provider.setting_options()) {
        Some(Ok(Some(options))) if offered(&options, harness, model) => None,
        Some(Err(_)) => Some((error(500, "the options could not be read"), "the options could not be read")),
        _ => not_offered(),
    }
}

/// What a planning run's start answers: the run, or why it did not start, audited as plan.start.
fn plan_started(audit: &dyn Fn(crate::remote_audit::Outcome, &str), label: &str, project: &str, started: Option<Result<PlanStart, String>>) -> Response {
    use crate::remote_audit::Outcome;
    match started {
        Some(Ok(PlanStart::Started(run))) => {
            audit(Outcome::Allowed, &format!("{label}: {run}"));
            reply(200, json!({ "run": run, "project": project }))
        }
        Some(Ok(PlanStart::Refused(why))) => {
            audit(Outcome::Refused, &format!("{label}: {why}"));
            reply(409, json!({ "error": why }))
        }
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(_)) => {
            audit(Outcome::Refused, &format!("{label}: the planning run could not start"));
            error(500, "the planning run could not start")
        }
        _ => {
            audit(Outcome::Refused, &format!("{label}: no such project"));
            error(404, "not found")
        }
    }
}

/// Draft with orq on Todo item `id`, if actions are on: a planning run in the project the body names, from the
/// item as read at the body's version. Audited as plan.start.
fn plan_from_todo(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: i64, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let sent = body_object(body, MAX_TODO_BODY);
    let project = sent.as_ref().ok().and_then(|sent| sent.get("project")).and_then(Value::as_str).unwrap_or_default().to_string();
    let audit = |outcome, reason: &str| {
        let _ = record(roots, &Attempt { identity: "web", action: "plan.start", project: &project, run: None, outcome, reason: Some(reason) }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, "remote actions are off");
        return error(404, "not found");
    }
    let sent = match sent {
        Ok(sent) => sent,
        Err(refusal) => {
            audit(Outcome::Refused, "bad request body");
            return refusal;
        }
    };
    const KEYS: [&str; 5] = ["project", "version", "notes", "harness", "model"];
    let (true, Some(read), Some(notes), Some((harness, model))) =
        (sent.keys().all(|key| KEYS.contains(&key.as_str())) && !project.is_empty(), sent.get("version").and_then(Value::as_i64), plan_notes(&sent), plan_agent(&sent))
    else {
        audit(Outcome::Refused, "bad request body");
        return error(400, "the request body is not one this action takes");
    };
    if let Some((refusal, why)) = plan_agent_refusal(provider, &harness, &model) {
        audit(Outcome::Refused, why);
        return refusal;
    }
    let item = if roots.database().exists() { Store::open(&roots.database()).and_then(|store| store.todo(id)) } else { Ok(None) };
    let item = match item {
        Ok(Some(item)) => item,
        Ok(None) => {
            audit(Outcome::Refused, "no such item");
            return error(404, "not found");
        }
        Err(_) => {
            audit(Outcome::Refused, "the Todo list could not be read");
            return error(500, "the Todo list could not be read");
        }
    };
    if read != item.version {
        audit(Outcome::Refused, &format!("{}: the item changed since it was read", item.name));
        return error(409, "The item changed since you opened it; nothing was started");
    }
    let request = PlanRequest { idea: PlanIdea::Todo { id, version: item.version, name: item.name.clone(), text: item.description.clone() }, notes, harness, model };
    plan_started(&audit, &item.name, &project, provider.map(|provider| provider.start_plan(&project, &request)))
}

/// Draft with orq on draft `feature` of project `id` ("Improve draft"), if actions are on. Audited as plan.start.
fn plan_from_draft(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, feature: &str, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let audit = |outcome, reason: &str| {
        let _ = record(roots, &Attempt { identity: "web", action: "plan.start", project: id, run: None, outcome, reason: Some(reason) }, now);
    };
    if !plain_feature_name(feature) {
        audit(Outcome::Refused, "not a feature name");
        return error(404, "not found");
    }
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, &format!("{feature}: remote actions are off"));
        return error(404, "not found");
    }
    let fields = match body_object(body, MAX_TODO_BODY) {
        Ok(sent) if sent.keys().all(|key| ["notes", "harness", "model"].contains(&key.as_str())) => plan_notes(&sent).zip(plan_agent(&sent)),
        _ => None,
    };
    let Some((notes, (harness, model))) = fields else {
        audit(Outcome::Refused, &format!("{feature}: bad request body"));
        return error(400, "the request body is not one this action takes");
    };
    if let Some((refusal, why)) = plan_agent_refusal(provider, &harness, &model) {
        audit(Outcome::Refused, &format!("{feature}: {why}"));
        return refusal;
    }
    let request = PlanRequest { idea: PlanIdea::Draft { feature: feature.to_string() }, notes, harness, model };
    plan_started(&audit, feature, id, provider.map(|provider| provider.start_plan(id, &request)))
}

/// Replace or Discard on planning run `run`'s improved draft, if actions are on; audited as plan.replace or
/// plan.discard.
fn decide_plan(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, run: &str, replace: bool, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let action = if replace { "plan.replace" } else { "plan.discard" };
    let audit = |outcome, reason: &str| {
        let _ = record(roots, &Attempt { identity: "web", action, project: id, run: Some(run), outcome, reason: Some(reason) }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, "remote actions are off");
        return error(404, "not found");
    }
    if !matches!(body_object(body, MAX_TODO_BODY), Ok(sent) if sent.is_empty()) {
        audit(Outcome::Refused, "bad request body");
        return error(400, "the request body is not one this action takes");
    }
    match provider.map(|provider| provider.decide_plan(id, run, replace)) {
        Some(Ok(PlanDecision::Done)) => {
            audit(Outcome::Allowed, if replace { "replaced" } else { "discarded" });
            reply(200, json!({ "run": run, "decision": if replace { "replaced" } else { "discarded" } }))
        }
        Some(Ok(PlanDecision::Refused(why))) => {
            audit(Outcome::Refused, &why);
            reply(409, json!({ "error": why }))
        }
        Some(Err(_)) => {
            audit(Outcome::Refused, "the decision could not be saved");
            error(500, "the decision could not be saved")
        }
        _ => {
            audit(Outcome::Refused, "no such planning run");
            error(404, "not found")
        }
    }
}

/// A request body as a JSON object of at most `max` bytes; an empty body is an empty object.
fn body_object(body: &[u8], max: usize) -> Result<serde_json::Map<String, Value>, Response> {
    if body.len() > max {
        return Err(error(413, "the request body is too large"));
    }
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(serde_json::Map::new());
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(fields)) => Ok(fields),
        _ => Err(error(400, "the request body is not one this action takes")),
    }
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

/// Pauses, resumes, stops, accepts, merges (setting changes aside) or builds the missing report of a run for the web, if actions are on, and leaves an audit line for the attempt.
fn control_run(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, run: &str, control: RunControl, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let (action, failed) = match control {
        RunControl::Pause => ("run.pause", "the run could not be paused"),
        RunControl::Resume => ("run.resume", "the run could not be resumed"),
        RunControl::ReviewAgain => ("run.review_again", "the run could not be resumed"),
        RunControl::Stop => ("run.stop", "the run could not be stopped"),
        RunControl::Accept => ("run.accept", "the run could not be accepted"),
        RunControl::MergeSetAside => ("run.merge_set_aside", "the run could not be merged"),
        RunControl::BuildReport => ("run.build_report", "the review report could not be built"),
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
    // Only a Resume takes a body: the person's answer, which can be longer than another action's body.
    let refusal = match control {
        RunControl::Resume => body_refusal_within(body, &["answer"], MAX_ACTION_BODY + 4 * MAX_QUEUED_ANSWER),
        _ => body_refusal(body, &[]),
    };
    let fields = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
    let answer = match fields.get("answer") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(answer)) => crate::store::run_queue::checked_answer(answer).map_err(drop),
        Some(_) => Err(()),
    };
    let Some(answer) = refusal.is_none().then_some(answer).and_then(Result::ok) else {
        audit(Outcome::Refused, Some(run), Some("bad request body"));
        return refusal.unwrap_or_else(|| error(400, "the request body is not one this action takes"));
    };
    // The audit line keeps the start of the answer, redacted on the way in, so the log says what was said.
    let summary = answer.map(|answer| format!("answer: {}", answer.split_whitespace().collect::<Vec<_>>().join(" ")));
    let outcome = match answer {
        Some(answer) => provider.map(|provider| provider.resume_with_answer(id, run, answer)),
        None => provider.map(|provider| provider.control_run(id, run, control)),
    };
    match outcome {
        Some(Ok(RunControlOutcome::Done(now_run))) => {
            audit(Outcome::Allowed, Some(run), summary.as_deref());
            reply(202, json!({ "run": now_run }))
        }
        Some(Ok(RunControlOutcome::Refused(why))) => {
            audit(Outcome::Refused, Some(run), Some(&why));
            reply(409, json!({ "error": why }))
        }
        // The reason can name a path on this machine, so the caller is only told it failed; the server's
        // own log, which only the operator reads, says why.
        Some(Err(why)) => {
            log_line(&launch_failure_line(action, id, run, &why));
            audit(Outcome::Refused, Some(run), Some(failed));
            error(500, failed)
        }
        _ => {
            audit(Outcome::Refused, None, Some("no such project or run"));
            error(404, "not found")
        }
    }
}

/// How much of a failure's reason the server log keeps.
const LOGGED_REASON_CAP: usize = 1000;

/// One line for the server's log when a run could not be started or controlled: the action, project and
/// feature or run, and the real reason, escaped so it stays on one line and cut short when long.
pub fn launch_failure_line(action: &str, project: &str, target: &str, why: &str) -> String {
    let shown = |text: &str, cap: usize| {
        let mut kept: String = text.chars().take(cap).flat_map(char::escape_default).collect();
        if text.chars().nth(cap).is_some() {
            kept.push('…');
        }
        kept
    };
    format!("orq-remote: {action} {} {} failed: {}", shown(project, 100), shown(target, 100), shown(why, LOGGED_REASON_CAP))
}

/// Writes `line` to the server's log, stderr, at best: a log that cannot be written never stops the request.
fn log_line(line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr().lock(), "{line}");
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

/// The project (when scoped), run and action of `/api/reviews/{run}/{action}` or
/// `/api/projects/{id}/reviews/{run}/{action}`, when the path has that shape and names one of the actions.
fn review_action_target(path: &str) -> Option<(Option<&str>, &str, ReviewAction)> {
    let (scope, rest) = match path.strip_prefix("/api/reviews/") {
        Some(rest) => (None, rest),
        None => {
            let (id, rest) = path.strip_prefix("/api/projects/")?.split_once('/')?;
            (Some(Some(id).filter(|id| !id.is_empty())?), rest.strip_prefix("reviews/")?)
        }
    };
    let (run, action) = rest.split_once('/')?;
    (!run.is_empty() && !action.contains('/')).then_some(())?;
    Some((scope, run, ReviewAction::parse(action)?))
}

/// Resolves, dismisses or reopens the review of a run for the web, if actions are on, and leaves an audit
/// line for the attempt. A `head` in the body must still be the head of the run's latest report.
fn review_action(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, scope: Option<&str>, run: &str, action: ReviewAction, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let name = match action {
        ReviewAction::Resolve => "review.resolve",
        ReviewAction::Dismiss => "review.dismiss",
        ReviewAction::Reopen => "review.reopen",
    };
    let failed = "the review could not be changed";
    let audit = |outcome, run: Option<&str>, reason: Option<&str>| {
        let _ = record(roots, &Attempt { identity: "web", action: name, project: scope.unwrap_or(""), run, outcome, reason }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, None, Some("remote actions are off"));
        return error(404, "not found");
    }
    if !plain_run_id(run) || check_run_id(run).is_err() {
        audit(Outcome::Refused, None, Some("not a run id"));
        return error(404, "not found");
    }
    if let Some(refusal) = body_refusal(body, &["head"]) {
        audit(Outcome::Refused, Some(run), Some("bad request body"));
        return refusal;
    }
    let head = match serde_json::from_slice::<Value>(body).ok().map(|body| body["head"].clone()) {
        None | Some(Value::Null) => None,
        Some(Value::String(head)) if (7..=64).contains(&head.len()) && head.bytes().all(|b| b.is_ascii_hexdigit()) => Some(head),
        Some(_) => {
            audit(Outcome::Refused, Some(run), Some("bad request body"));
            return error(400, "the head is not a commit");
        }
    };
    let not_found = || {
        audit(Outcome::Refused, Some(run), Some("no such review"));
        error(404, "not found")
    };
    let owner = match scope {
        None => None,
        Some(id) => match provider.map(|provider| provider.review_scope(id)) {
            Some(Ok(Some(scope))) => Some(scope.review_key),
            Some(Err(_)) => {
                audit(Outcome::Refused, Some(run), Some(failed));
                return error(500, failed);
            }
            _ => return not_found(),
        },
    };
    // A missing index is no review; it is never created by a write.
    if !roots.database().exists() {
        return not_found();
    }
    let store = match Store::open(&roots.database()) {
        Ok(store) => store,
        Err(_) => {
            audit(Outcome::Refused, Some(run), Some(failed));
            return error(500, failed);
        }
    };
    let project = match store.reviews_for_run(run) {
        Ok(records) => match records.into_iter().last() {
            Some(record) if owner.as_ref().is_none_or(|owner| *owner == record.project) => record.project,
            _ => return not_found(),
        },
        Err(_) => {
            audit(Outcome::Refused, Some(run), Some(failed));
            return error(500, failed);
        }
    };
    match review_inbox::act_on_review(&store, run, action, head.as_deref()) {
        Ok(ActionOutcome::Done(lifecycle)) => {
            let head = lifecycle.reported_head_sha;
            if let Some(Err(why)) = provider.map(|provider| provider.review_acted(&project, run, action, &head)) {
                log_line(&launch_failure_line(name, &project, run, &why));
            }
            audit(Outcome::Allowed, Some(run), None);
            let status = review_inbox::status_of_run(roots, run).ok().flatten();
            reply(200, json!({ "run": run, "status": status, "head": head }))
        }
        Ok(ActionOutcome::StaleHead { latest }) => {
            let why = "a newer report of this run was written; reload it first";
            audit(Outcome::Refused, Some(run), Some(why));
            reply(409, json!({ "error": why, "latest": latest }))
        }
        Ok(ActionOutcome::Refused(why)) => {
            audit(Outcome::Refused, Some(run), Some(&why));
            reply(409, json!({ "error": why }))
        }
        Ok(ActionOutcome::NoReview) => not_found(),
        Err(why) => {
            log_line(&launch_failure_line(name, &project, run, &why));
            audit(Outcome::Refused, Some(run), Some(failed));
            error(500, failed)
        }
    }
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

/// Cleans up a merged run for the web, as its Clean up card asks: only with actions on, from a passkey
/// session (`passkey`), and with the press confirmed (`{"confirm": true}`). Every attempt is audited as
/// `run.cleanup`.
fn clean_up_run(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, run: &str, body: &[u8], passkey: bool) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let failed = "the run could not be cleaned up";
    let audit = |outcome, run: Option<&str>, reason: Option<&str>| {
        let _ = record(roots, &Attempt { identity: "web", action: "run.cleanup", project: id, run, outcome, reason }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, None, Some("remote actions are off"));
        return error(404, "not found");
    }
    if !plain_run_id(run) {
        audit(Outcome::Refused, None, Some("not a run id"));
        return error(404, "not found");
    }
    if let Some(refusal) = body_refusal(body, &["confirm"]) {
        audit(Outcome::Refused, Some(run), Some("bad request body"));
        return refusal;
    }
    if !passkey {
        audit(Outcome::Refused, Some(run), Some("no passkey session"));
        return error(403, "cleaning up a run needs a passkey sign-in");
    }
    if serde_json::from_slice::<Value>(body).ok().and_then(|fields| fields["confirm"].as_bool()) != Some(true) {
        audit(Outcome::Refused, Some(run), Some("not confirmed"));
        return error(400, "cleaning up a run needs it confirmed");
    }
    match provider.map(|provider| provider.clean_up_run(id, run)) {
        Some(Ok(RunCleanup::Done(card))) => {
            audit(Outcome::Allowed, Some(run), None);
            reply(200, card)
        }
        Some(Ok(RunCleanup::Refused(why))) => {
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

/// The most a design issues post may hold: the issues file `orq run resume --design-issues` reads, and its wrapping.
const MAX_DESIGN_ISSUES_BODY: usize = 256 * 1024 + 1024;

/// Resumes a frontend run with a person's design issues for the web, if actions are on, and leaves an audit
/// line for the attempt as run.design_issues. The body is `{"issues": [...]}`; the issues themselves are
/// the provider's to check, and are never written to the audit.
fn resume_with_design_issues(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, run: &str, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let failed = "the run could not be resumed";
    let audit = |outcome, run: Option<&str>, reason: Option<&str>| {
        let _ = record(roots, &Attempt { identity: "web", action: "run.design_issues", project: id, run, outcome, reason }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, None, Some("remote actions are off"));
        return error(404, "not found");
    }
    if !plain_run_id(run) {
        audit(Outcome::Refused, None, Some("not a run id"));
        return error(404, "not found");
    }
    let issues = match body_object(body, MAX_DESIGN_ISSUES_BODY) {
        Ok(fields) if fields.len() == 1 && fields.get("issues").is_some_and(Value::is_array) => fields["issues"].to_string(),
        Ok(_) => {
            audit(Outcome::Refused, Some(run), Some("bad request body"));
            return error(400, "the request body is not one this action takes");
        }
        Err(refused) => {
            audit(Outcome::Refused, Some(run), Some("bad request body"));
            return refused;
        }
    };
    match provider.map(|provider| provider.resume_with_design_issues(id, run, &issues)) {
        Some(Ok(RunControlOutcome::Done(now_run))) => {
            audit(Outcome::Allowed, Some(run), None);
            reply(202, json!({ "run": now_run }))
        }
        // The reason can quote what the person typed (a viewport, a severity, a field's name), so the
        // audit keeps only that it was refused; the person is told why.
        Some(Ok(RunControlOutcome::Refused(why))) => {
            audit(Outcome::Refused, Some(run), Some("the run or its issues were refused"));
            reply(409, json!({ "error": why }))
        }
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(why)) => {
            log_line(&launch_failure_line("run.design_issues", id, run, &why));
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

/// The project and action of `/api/projects/{id}/queue` (add) or `/api/projects/{id}/queue/{action}`.
fn queue_target(path: &str) -> Option<(&str, &str)> {
    if let Some(id) = project_target(path, "queue") {
        return Some((id, "add"));
    }
    ["remove", "reorder", "pause", "resume"].into_iter().find_map(|leaf| {
        let id = path.strip_prefix("/api/projects/")?.strip_suffix(leaf)?.strip_suffix("/queue/")?;
        (!id.is_empty() && !id.contains('/')).then_some((id, leaf))
    })
}

/// Pulls a project's target branch again, as the Retry after a merge that could not be pulled asks:
/// only with actions on, and every attempt audited as `project.pull`.
fn pull_project_target(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let audit = |outcome, reason: &str| {
        let _ = record(roots, &Attempt { identity: "web", action: "project.pull", project: id, run: None, outcome, reason: Some(reason) }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, "remote actions are off");
        return error(404, "not found");
    }
    if let Some(refusal) = body_refusal(body, &[]) {
        audit(Outcome::Refused, "bad request body");
        return refusal;
    }
    match provider.map(|provider| provider.pull_project_target(id)) {
        Some(Ok(Some(pulled))) => {
            audit(Outcome::Allowed, pulled.get("message").and_then(Value::as_str).unwrap_or_default());
            reply(200, pulled)
        }
        Some(Err(why)) => {
            audit(Outcome::Refused, &why);
            error(500, "the target could not be pulled")
        }
        _ => {
            audit(Outcome::Refused, "no such project");
            error(404, "not found")
        }
    }
}

/// Whether a project's target may be pushed now, why not, and the commits a push would send, for the
/// web's Push. It changes nothing, so it is there with actions off too, and it says whether they are on.
fn push_target_state(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str) -> Response {
    match provider.map(|provider| provider.push_target_state(id)) {
        Some(Ok(Some(mut state))) => {
            state["remote_actions"] = json!(remote_actions_on(roots));
            reply(200, state)
        }
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(_)) => error(500, "the target could not be checked"),
        _ => error(404, "not found"),
    }
}

/// Pushes a project's target to origin at the confirmed `{sha}`, as the web's Push asks: only with
/// actions on and from a passkey session (`passkey`). Every attempt is audited as `project.push_target`,
/// a push with its branch, the shas it moved origin between, its count and that it came from the web.
fn push_project_target(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, body: &[u8], passkey: bool) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let audit = |outcome, reason: &str| {
        let _ = record(roots, &Attempt { identity: "web", action: "project.push_target", project: id, run: None, outcome, reason: Some(reason) }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, "remote actions are off");
        return error(404, "not found");
    }
    if let Some(refusal) = body_refusal(body, &["sha"]) {
        audit(Outcome::Refused, "bad request body");
        return refusal;
    }
    let sha = serde_json::from_slice::<Value>(body).ok().and_then(|fields| fields["sha"].as_str().map(str::to_string)).unwrap_or_default();
    if !((sha.len() == 40 || sha.len() == 64) && sha.chars().all(|c| c.is_ascii_hexdigit())) {
        audit(Outcome::Refused, "not a commit id");
        return error(400, "pushing the target needs the commit it was confirmed at");
    }
    if !passkey {
        audit(Outcome::Refused, "no passkey session");
        return error(403, "pushing the target needs a passkey sign-in");
    }
    match provider.map(|provider| provider.push_target(id, &sha)) {
        Some(Ok(TargetPushResult::Pushed { target, from, to, count, runs })) => {
            let noun = if count == 1 { "commit" } else { "commits" };
            audit(Outcome::Allowed, &format!("pushed {target} from {from} to {to}, {count} {noun}, from web"));
            reply(200, json!({ "from": from, "to": to, "count": count, "runs": runs }))
        }
        Some(Ok(TargetPushResult::Refused(why))) => {
            audit(Outcome::Refused, &why);
            reply(409, json!({ "error": why }))
        }
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(_)) => {
            audit(Outcome::Refused, "the target could not be pushed");
            error(500, "the target could not be pushed")
        }
        _ => {
            audit(Outcome::Refused, "no such project");
            error(404, "not found")
        }
    }
}

/// Adds to, removes from, reorders, pauses or resumes a project's run queue: only with actions on,
/// and every attempt audited as `queue.<action>`.
fn change_queue(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, leaf: &str, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let action = format!("queue.{leaf}");
    let audit = |outcome, reason: &str| {
        let _ = record(roots, &Attempt { identity: "web", action: &action, project: id, run: None, outcome, reason: Some(reason) }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, "remote actions are off");
        return error(404, "not found");
    }
    let keys: &[&str] = match leaf {
        "add" => &["features", "implementer", "reviewer", "action", "run", "answer"],
        "remove" => &["feature"],
        "reorder" => &["order"],
        _ => &[],
    };
    // A queued Resume's answer can be longer than another action's body.
    if let Some(refusal) = body_refusal_within(body, keys, MAX_ACTION_BODY + 4 * MAX_QUEUED_ANSWER) {
        audit(Outcome::Refused, "bad request body");
        return refusal;
    }
    let fields = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
    let names = |key: &str, valid: fn(&str) -> bool| -> Option<Vec<String>> {
        let list = fields.get(key)?.as_array()?;
        let names: Vec<String> = list.iter().map(|name| name.as_str().filter(|name| !name.is_empty() && valid(name)).map(str::to_string)).collect::<Option<_>>()?;
        (!names.is_empty() && names.len() <= 200).then_some(names)
    };
    let change = match leaf {
        "add" if fields.get("action").is_some() => queued_action(&fields).map(QueueChange::AddAction),
        "add" => {
            let text = |key: &str| fields.get(key).and_then(Value::as_str).map(str::to_string);
            let profiles = match (text("implementer"), text("reviewer"), fields.get("implementer").is_some() || fields.get("reviewer").is_some()) {
                (Some(implementer), Some(reviewer), _) => Some(Some(LaunchProfiles { implementer, reviewer })),
                (_, _, true) => None,
                _ => Some(None),
            };
            names("features", plain_feature_name).zip(profiles).map(|(features, profiles)| QueueChange::Add { features, profiles })
        }
        "remove" => fields.get("feature").and_then(Value::as_str).filter(|name| queue_entry_name(name)).map(|name| QueueChange::Remove(name.to_string())),
        "reorder" => names("order", queue_entry_name).map(QueueChange::Reorder),
        "pause" => Some(QueueChange::Pause),
        _ => Some(QueueChange::Resume),
    };
    let Some(change) = change else {
        audit(Outcome::Refused, "bad request body");
        return error(400, "the request body is not one this action takes");
    };
    let named = match &change {
        QueueChange::Add { features, profiles: Some(p) } => format!("{}; implementer {}, reviewer {}", features.join(", "), p.implementer, p.reviewer),
        QueueChange::Add { features, profiles: None } => features.join(", "),
        // The answer is the person's own text, so the audit names only the action.
        QueueChange::AddAction(action) => action.entry_name(),
        QueueChange::Remove(feature) => feature.clone(),
        QueueChange::Reorder(order) => order.join(", "),
        QueueChange::Pause | QueueChange::Resume => String::new(),
    };
    let failed = "the queue could not be changed";
    match provider.map(|provider| provider.change_queue(id, &change)) {
        Some(Ok(RunControlOutcome::Done(done))) => {
            audit(Outcome::Allowed, if named.is_empty() { &done } else { &named });
            match provider.map(|provider| provider.queue(id)) {
                Some(Ok(Some(queue))) => reply(200, json!({ "queue": queue })),
                _ => reply(200, json!({})),
            }
        }
        Some(Ok(RunControlOutcome::Refused(why))) => {
            audit(Outcome::Refused, &why);
            reply(409, json!({ "error": why }))
        }
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(_)) => {
            audit(Outcome::Refused, failed);
            error(500, failed)
        }
        _ => {
            audit(Outcome::Refused, "no such project");
            error(404, "not found")
        }
    }
}

/// The run action a queue addition's body asks for: `action` (`resume`, `review_again` or `accept`)
/// on `run`, with a Resume's optional `answer`; `None` when the body is not one of those.
fn queued_action(fields: &Value) -> Option<crate::store::run_queue::QueueAction> {
    use crate::store::run_queue::{QueueAction, QueueKind};
    if ["features", "implementer", "reviewer"].iter().any(|key| fields.get(key).is_some()) {
        return None;
    }
    let kind = QueueKind::parse(fields.get("action")?.as_str()?)?;
    let run = fields.get("run")?.as_str().filter(|run| !run.is_empty() && plain_run_id(run))?.to_string();
    let answer = match fields.get("answer") {
        None | Some(Value::Null) => None,
        Some(answer) => crate::store::run_queue::checked_answer(answer.as_str()?).ok()?.map(str::to_string),
    };
    if answer.is_some() && kind != QueueKind::Resume {
        return None;
    }
    Some(QueueAction { kind, run, answer })
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

/// What a storage cleanup would remove, for the web's Settings. It changes nothing, so it is not audited,
/// but it is there only while actions are on.
fn storage_preview(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>) -> Response {
    action_read(roots, provider, "the cleanup could not be previewed", |provider| provider.storage_preview())
}

/// Removes the previewed cleanup set for the web, if actions are on and the set still has the hash
/// previewed, and leaves an audit line for the attempt as storage.prune.
fn storage_prune(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, body: &[u8]) -> Response {
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let audit = |outcome, reason: &str| {
        let _ = record(roots, &Attempt { identity: "web", action: "storage.prune", project: "settings", run: None, outcome, reason: Some(reason) }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, "remote actions are off");
        return error(404, "not found");
    }
    if let Some(refusal) = body_refusal(body, &["hash"]) {
        audit(Outcome::Refused, "bad request body");
        return refusal;
    }
    let fields = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
    let Some(hash) = fields["hash"].as_str().filter(|hash| !hash.is_empty()) else {
        audit(Outcome::Refused, "bad request body");
        return error(400, "the request body is not one this action takes");
    };
    let failed = "the cleanup could not be done";
    let conflict = |why: &str| {
        audit(Outcome::Refused, why);
        reply(409, json!({ "error": why }))
    };
    match provider.map(|provider| provider.storage_prune(hash)) {
        Some(Ok(Some(StoragePrune::Pruned(lines)))) => {
            audit(Outcome::Allowed, lines.first().map_or("", String::as_str));
            reply(200, json!({ "lines": lines }))
        }
        Some(Ok(Some(StoragePrune::Changed))) => conflict("The cleanup changed since the preview; nothing was removed. Preview it again."),
        Some(Ok(Some(StoragePrune::Refused(why)))) => conflict(&why),
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(_)) => {
            audit(Outcome::Refused, failed);
            error(500, failed)
        }
        _ => {
            audit(Outcome::Refused, "this server cannot clean up");
            error(404, "not found")
        }
    }
}

/// The kept worktrees of every project, for the Storage card. It changes nothing, so it is not audited, but
/// it is there only while actions are on.
fn kept_worktrees(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>) -> Response {
    action_read(roots, provider, "the kept worktrees could not be read", |provider| provider.kept_worktrees())
}

/// The project and branch of `/api/storage/worktrees/{project}/{branch}/delete`; the branch may hold `/`.
fn worktree_target(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("/api/storage/worktrees/")?.strip_suffix("/delete")?;
    let (project, branch) = rest.split_once('/')?;
    (!project.is_empty() && !branch.is_empty() && !branch.split('/').any(|part| part.is_empty() || part == "." || part == "..")).then_some((project, branch))
}

fn worktree_audit(roots: &StorageRoots, action: &str, project: &str, outcome: crate::remote_audit::Outcome, reason: &str) {
    use crate::remote_audit::{record, Attempt};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let _ = record(roots, &Attempt { identity: "web", action, project, run: None, outcome, reason: Some(reason) }, now);
}

/// One delete through the provider, as `(status, body)`, with its audit line.
fn delete_one(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, project: &str, branch: &str, version: &str, force: bool, action: &str) -> (u16, Value) {
    let answer = provider.map(|provider| provider.delete_kept_worktree(project, branch, version, force));
    delete_answer(roots, project, branch, answer, action)
}

/// What the provider said of one delete, as `(status, body)`, with its audit line.
fn delete_answer(roots: &StorageRoots, project: &str, branch: &str, answer: Option<Result<WorktreeDelete, String>>, action: &str) -> (u16, Value) {
    use crate::remote_audit::Outcome;
    let failed = "the worktree could not be deleted";
    match answer {
        Some(Ok(WorktreeDelete::Deleted(deleted))) => {
            worktree_audit(roots, action, project, Outcome::Allowed, &format!("deleted {branch}"));
            (200, deleted)
        }
        Some(Ok(WorktreeDelete::Refused(why))) => {
            worktree_audit(roots, action, project, Outcome::Refused, &why);
            (409, json!({ "error": why }))
        }
        // The reason can name a path on this machine, so the caller is only told it failed.
        Some(Err(_)) => {
            worktree_audit(roots, action, project, Outcome::Refused, failed);
            (500, json!({ "error": failed }))
        }
        _ => {
            worktree_audit(roots, action, project, Outcome::Refused, "no such kept worktree");
            (404, json!({ "error": "not found" }))
        }
    }
}

/// Deletes one kept worktree for the web, if actions are on, as storage.worktree.delete.
fn delete_kept_worktree(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, path: &str, body: &[u8]) -> Response {
    use crate::remote_audit::Outcome;
    let action = "storage.worktree.delete";
    if !remote_actions_on(roots) {
        worktree_audit(roots, action, "settings", Outcome::Refused, "remote actions are off");
        return error(404, "not found");
    }
    let Some((project, branch)) = worktree_target(path) else {
        worktree_audit(roots, action, "settings", Outcome::Refused, "no such kept worktree");
        return error(404, "not found");
    };
    if let Some(refusal) = body_refusal(body, &["version", "force"]) {
        worktree_audit(roots, action, project, Outcome::Refused, "bad request body");
        return refusal;
    }
    let fields = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
    let force = match fields["force"] {
        Value::Null => Some(false),
        Value::Bool(force) => Some(force),
        _ => None,
    };
    let (Some(version), Some(force)) = (fields["version"].as_str().filter(|version| !version.is_empty()), force) else {
        worktree_audit(roots, action, project, Outcome::Refused, "bad request body");
        return error(400, "the request body is not one this action takes");
    };
    let (status, body) = delete_one(roots, provider, project, branch, version, force, action);
    reply(status, body)
}

/// The most worktrees one delete-merged takes.
const MAX_MERGED_DELETES: usize = 200;
/// The largest delete-merged body: room for that many worktrees' project, branch and version.
const MAX_MERGED_BODY: usize = 64 * 1024;

/// Deletes each listed kept worktree that is still merged with nothing to lose, never forcing one, and says
/// how each went, as storage.worktree.delete-merged.
fn delete_merged_worktrees(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, body: &[u8]) -> Response {
    use crate::remote_audit::Outcome;
    let action = "storage.worktree.delete-merged";
    if !remote_actions_on(roots) {
        worktree_audit(roots, action, "settings", Outcome::Refused, "remote actions are off");
        return error(404, "not found");
    }
    if let Some(refusal) = body_refusal_within(body, &["worktrees"], MAX_MERGED_BODY) {
        worktree_audit(roots, action, "settings", Outcome::Refused, "bad request body");
        return refusal;
    }
    let fields = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
    let items: Option<Vec<(&str, &str, &str)>> = fields["worktrees"].as_array().filter(|items| !items.is_empty() && items.len() <= MAX_MERGED_DELETES).and_then(|items| {
        items
            .iter()
            .map(|item| {
                let field = |name: &str| item[name].as_str().filter(|text| !text.is_empty());
                let known = item.as_object()?.keys().all(|key| matches!(key.as_str(), "project" | "branch" | "version"));
                known.then_some((field("project")?, field("branch")?, field("version")?))
            })
            .collect()
    });
    let Some(items) = items else {
        worktree_audit(roots, action, "settings", Outcome::Refused, "bad request body");
        return error(400, "the request body is not one this action takes");
    };
    let mut answers = provider.map(|provider| provider.delete_kept_worktrees(&items, false)).unwrap_or_default().into_iter();
    let results: Vec<Value> = items
        .into_iter()
        .map(|(project, branch, _)| {
            let (status, answer) = delete_answer(roots, project, branch, answers.next(), action);
            let mut result = json!({ "project": project, "branch": branch, "deleted": status == 200 });
            if status != 200 {
                result["error"] = answer["error"].clone();
            }
            result
        })
        .collect();
    reply(200, json!({ "results": results }))
}

/// Saves project `id`'s own agents, delivery and run limits for its Settings page. The body names the
/// whole record and the revision it was read at; a null role is reset so runs fall back again, and a
/// null limit is no limit.
/// Saves project `id`'s settings; `passkey` is whether the request came from a passkey session, which
/// turning `after_merge` to `auto` needs, as Clean up itself does.
fn save_project_settings(roots: &StorageRoots, provider: Option<&dyn ProjectsProvider>, id: &str, body: &[u8], passkey: bool) -> Response {
    use crate::project_settings::{AfterMerge, Delivery, ProjectAgent, ProjectSettings};
    use crate::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let audit = |outcome, reason: Option<&str>| {
        let _ = record(roots, &Attempt { identity: "web", action: "project.settings", project: id, run: None, outcome, reason }, now);
    };
    if !remote_actions_on(roots) {
        audit(Outcome::Refused, Some("remote actions are off"));
        return error(404, "not found");
    }
    const KEYS: [&str; 14] =["implementer", "reviewer", "delivery", "max_active_runs", "min_free_disk_gb", "queue_hours", "queue_stop_on_failure", "queue_wait_for_review", "pull_target_before_runs", "after_merge", "design_preparer", "design_reviewer", "design_verifier", "revision"];
    if let Some(refusal) = body_refusal(body, &KEYS) {
        audit(Outcome::Refused, Some("bad request body"));
        return refusal;
    }
    let fields = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
    // A role is null (reset) or exactly {harness, model} with an optional effort, a string or null.
    let agent = |role: &str| -> Option<Option<ProjectAgent>> {
        match fields.get(role)? {
            Value::Null => Some(None),
            Value::Object(agent) if agent.keys().all(|key| ["harness", "model", "effort"].contains(&key.as_str())) => {
                let effort = match agent.get("effort") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(level)) => Some(level.clone()),
                    Some(_) => return None,
                };
                let text = |key: &str| agent.get(key).and_then(Value::as_str).map(str::to_string);
                Some(Some(ProjectAgent { harness: text("harness")?, model: text("model")?, effort }))
            }
            _ => None,
        }
    };
    // A limit is null (no limit) or a whole number; its range is checked once the body is read.
    let limit = |key: &str| -> Option<Option<u64>> {
        match fields.get(key)? {
            Value::Null => Some(None),
            value => value.as_u64().map(Some),
        }
    };
    let parsed = (agent("implementer"), agent("reviewer"), fields.get("delivery").and_then(Value::as_str).and_then(Delivery::parse), fields.get("revision").and_then(Value::as_u64));
    let (Some(implementer), Some(reviewer), Some(delivery), Some(revision), Some(max_active_runs), Some(min_free_disk_gb)) =
        (parsed.0, parsed.1, parsed.2, parsed.3, limit("max_active_runs"), limit("min_free_disk_gb"))
    else {
        audit(Outcome::Refused, Some("bad request body"));
        return error(400, "the request body is not one this action takes");
    };
    if delivery == Delivery::Push {
        audit(Outcome::Refused, Some("the push delivery was removed"));
        return error(422, crate::project_settings::PUSH_REMOVED);
    }
    use crate::project_settings::{MAX_ACTIVE_RUNS, MIN_FREE_DISK_GB};
    let Ok(max_active_runs) = max_active_runs.map(|limit| u8::try_from(limit).ok().filter(|limit| MAX_ACTIVE_RUNS.contains(limit)).ok_or(())).transpose() else {
        audit(Outcome::Refused, Some("run limit out of range"));
        return error(422, "At most active runs must be empty or 1 to 8");
    };
    let Ok(min_free_disk_gb) = min_free_disk_gb.map(|floor| u32::try_from(floor).ok().filter(|floor| MIN_FREE_DISK_GB.contains(floor)).ok_or(())).transpose() else {
        audit(Outcome::Refused, Some("disk floor out of range"));
        return error(422, "Free disk needed to start must be empty or 1 to 500 GB");
    };
    // A design role left out keeps what is stored, as an older page that knows none of them saves; null resets
    // it. A kept role was checked when it was saved, so only the roles sent are checked here.
    let left_out = ["design_preparer", "design_reviewer", "design_verifier"].map(|role| fields.get(role).is_none());
    let design_role = |index: usize, role: &str| if left_out[index] { Some(None) } else { agent(role) };
    let (Some(design_preparer), Some(design_reviewer), Some(design_verifier)) = (design_role(0, "design_preparer"), design_role(1, "design_reviewer"), design_role(2, "design_verifier")) else {
        audit(Outcome::Refused, Some("bad request body"));
        return error(400, "the request body is not one this action takes");
    };
    let stored_design = if left_out.contains(&true) {
        match provider.map(|provider| provider.design_agents(id)) {
            Some(Err(_)) => {
                audit(Outcome::Refused, Some("the stored settings could not be read"));
                return error(500, "the stored settings could not be read");
            }
            Some(Ok(Some(stored))) => stored,
            _ => [None, None, None],
        }
    } else {
        [None, None, None]
    };
    let design = [("Design preparer", &design_preparer), ("Design reviewer", &design_reviewer), ("Design verifier", &design_verifier)];
    let chosen: Vec<&ProjectAgent> = implementer.iter().chain(reviewer.iter()).collect();
    // Resetting every role names no agent, so it needs nothing offered.
    if !chosen.is_empty() || design.iter().any(|(_, agent)| agent.is_some()) {
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
        if !chosen.iter().all(|agent| offered(&options, &agent.harness, &agent.model)) {
            audit(Outcome::Refused, Some("not an offered harness or model"));
            return error(422, "That harness or model is not one this machine offers");
        }
        let takes = |agent: &&ProjectAgent| agent.effort.as_deref().is_none_or(|level| efforts(&options, &agent.harness).is_some_and(|levels| levels.contains(&level)));
        if !chosen.iter().all(takes) {
            audit(Outcome::Refused, Some("not an offered effort"));
            return error(422, "That effort is not one this harness takes");
        }
        // pi is asked for its models once, and only when a design role uses it.
        let pi_models = if design.iter().any(|(_, agent)| agent.as_ref().is_some_and(|agent| agent.harness == "pi")) {
            match provider.map(|provider| provider.pi_models()) {
                Some(Err(_)) => {
                    audit(Outcome::Refused, Some("pi's models could not be listed"));
                    return error(500, "pi's models could not be listed");
                }
                Some(Ok(listing)) => listing,
                None => None,
            }
        } else {
            None
        };
        for (title, agent) in design {
            let Some(agent) = agent else { continue };
            let levels = efforts(&options, &agent.harness).unwrap_or_default();
            if let Some(refusal) = crate::project_settings::design_agent_refusal(title, agent, &levels, pi_models.as_deref()) {
                audit(Outcome::Refused, Some("not a design agent"));
                return error(422, &refusal);
            }
            // pi's own listing says which of its models a design role may use; any other model is one offered here.
            if agent.harness != "pi" && !offered(&options, &agent.harness, &agent.model) {
                audit(Outcome::Refused, Some("not an offered harness or model"));
                return error(422, "That harness or model is not one this machine offers");
            }
        }
    }
    // The queue's window is absent or null (any time) or `HH:MM-HH:MM`; stopping on failure is off unless true.
    let queue_hours = match fields.get("queue_hours") {
        None | Some(Value::Null) => None,
        Some(Value::String(hours)) if crate::project_settings::parse_queue_hours(hours).is_some() => Some(hours.clone()),
        Some(_) => {
            audit(Outcome::Refused, Some("queue hours not understood"));
            return error(422, "Queue hours must be empty or HH:MM-HH:MM, such as 22:00-07:00");
        }
    };
    let Some(queue_stop_on_failure) = fields.get("queue_stop_on_failure").map_or(Some(false), Value::as_bool) else {
        audit(Outcome::Refused, Some("bad request body"));
        return error(400, "the request body is not one this action takes");
    };
    // Waiting for each run's review is on unless false, as it is by default.
    let Some(queue_wait_for_review) = fields.get("queue_wait_for_review").map_or(Some(true), Value::as_bool) else {
        audit(Outcome::Refused, Some("bad request body"));
        return error(400, "the request body is not one this action takes");
    };
    // Pulling the target before runs is on unless false, as it is by default.
    let Some(pull_target_before_runs) = fields.get("pull_target_before_runs").map_or(Some(true), Value::as_bool) else {
        audit(Outcome::Refused, Some("bad request body"));
        return error(400, "the request body is not one this action takes");
    };
    // After a merge the server asks unless told `auto`, as it does by default.
    let after_merge = fields.get("after_merge").map_or(Some(AfterMerge::Ask), |value| value.as_str().and_then(AfterMerge::parse));
    let Some(after_merge) = after_merge else {
        audit(Outcome::Refused, Some("bad request body"));
        return error(400, "the request body is not one this action takes");
    };
    // Cleaning up on its own does what only a passkey session may press Clean up for, so only one turns it on.
    if after_merge.as_str() == "auto" && !passkey {
        let already = provider.and_then(|provider| provider.project_settings(id).ok().flatten()).is_some_and(|current| current["after_merge"] == "auto");
        if !already {
            audit(Outcome::Refused, Some("after_merge auto needs a passkey"));
            return error(403, "Cleaning up on its own after a merge needs a passkey sign-in");
        }
    }
    let [stored_preparer, stored_reviewer, stored_verifier] = stored_design;
    let keep = |index: usize, sent: Option<ProjectAgent>, stored: Option<ProjectAgent>| if left_out[index] { stored } else { sent };
    let (design_preparer, design_reviewer, design_verifier) = (keep(0, design_preparer, stored_preparer), keep(1, design_reviewer, stored_reviewer), keep(2, design_verifier, stored_verifier));
    let settings = ProjectSettings { implementer, reviewer, delivery, max_active_runs, min_free_disk_gb, queue_hours, queue_stop_on_failure, queue_wait_for_review, pull_target_before_runs, after_merge, design_preparer, design_reviewer, design_verifier };
    match provider.map(|provider| provider.save_project_settings(id, revision, &settings)) {
        Some(Ok(ProjectSettingsSave::Saved(settings))) => {
            audit(Outcome::Allowed, None);
            reply(200, json!({ "settings": settings }))
        }
        Some(Ok(ProjectSettingsSave::Conflict)) => {
            audit(Outcome::Refused, Some("settings changed"));
            error(409, "The project's settings changed since they were read")
        }
        // The reason can name a path on this machine, so neither the caller nor the audit is told it.
        Some(Err(_)) => {
            audit(Outcome::Refused, Some("the project's settings could not be saved"));
            error(500, "the project's settings could not be saved")
        }
        _ => {
            audit(Outcome::Refused, Some("unknown project"));
            error(404, "not found")
        }
    }
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
    if let Some((id, tail)) = crate::ask_orq::target(path) {
        return crate::ask_orq::route(roots, provider, id, &tail, request.method, body, confirm);
    }
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
    if let Some((scope, run, action)) = (request.method == "POST").then(|| review_action_target(path)).flatten() {
        return review_action(roots, provider, scope, run, action, body);
    }
    if let Some((id, run)) = (request.method == "POST").then(|| run_target(path, "discard")).flatten() {
        return discard_run(roots, provider, id, run, body);
    }
    if let Some((id, run)) = (request.method == "POST").then(|| run_target(path, "cleanup")).flatten() {
        return clean_up_run(roots, provider, id, run, body, confirm.is_some());
    }
    if let Some((id, run)) = (request.method == "POST").then(|| run_target(path, "model")).flatten() {
        return switch_model(roots, provider, id, run, body);
    }
    if let Some((id, run)) = (request.method == "POST").then(|| run_target(path, "design-issues")).flatten() {
        return resume_with_design_issues(roots, provider, id, run, body);
    }
    if let Some((id, feature)) = feature_target(path, "queue-wish").filter(|_| matches!(request.method, "GET" | "HEAD" | "POST")) {
        return queue_wish(roots, provider, id, feature, request.method, body);
    }
    if let Some(id) = (request.method == "POST").then(|| project_target(path, "features/archive")).flatten() {
        return archive_features(roots, provider, id, body);
    }
    if let Some((id, feature)) = (request.method == "POST").then(|| feature_target(path, "promote")).flatten() {
        return promote_feature(roots, provider, id, feature, body);
    }
    if let Some((id, feature)) = (request.method == "POST").then(|| feature_target(path, "kind")).flatten() {
        return set_feature_kind(roots, provider, id, feature, body);
    }
    if let Some(id) = (request.method == "POST").then(|| project_target(path, "features")).flatten() {
        return save_feature(roots, provider, id, None, body);
    }
    if let Some((id, feature)) = (request.method == "POST").then(|| feature_path(path)).flatten() {
        return save_feature(roots, provider, id, Some(feature), body);
    }
    if path == "/api/storage/worktrees/delete-merged" && request.method == "POST" {
        return delete_merged_worktrees(roots, provider, body);
    }
    if path.starts_with("/api/storage/worktrees/") && request.method == "POST" {
        return delete_kept_worktree(roots, provider, path, body);
    }
    if path == "/api/settings/storage/prune" && request.method == "POST" {
        return storage_prune(roots, provider, body);
    }
    // Not behind the remote_actions gate: turning actions off must always be possible.
    if path == "/api/settings/remote-actions" && request.method == "POST" {
        return set_remote_actions(roots, body, confirm);
    }
    // Not behind the remote_actions gate, like turning actions off: holding a restart back is always possible.
    if path == "/api/server/restart/cancel" && request.method == "POST" {
        return cancel_restart(roots, provider, body);
    }
    if path == "/api/server/restart" && request.method == "POST" {
        return restart_server(roots, provider, body, confirm.is_some());
    }
    // Not behind the remote_actions gate: how long a sign-in lasts is about the web itself.
    if path == "/api/settings/sessions" && request.method == "POST" {
        return set_session_settings(roots, body, confirm.is_some());
    }
    if path == "/api/settings/agents" && request.method == "POST" {
        return set_agents(roots, provider, body, confirm);
    }
    if let Some(id) = (request.method == "POST").then(|| project_target(path, "settings")).flatten() {
        return save_project_settings(roots, provider, id, body, confirm.is_some());
    }
    if let Some((id, leaf)) = (request.method == "POST").then(|| queue_target(path)).flatten() {
        return change_queue(roots, provider, id, leaf, body);
    }
    if let Some(id) = (request.method == "POST").then(|| project_target(path, "pull")).flatten() {
        return pull_project_target(roots, provider, id, body);
    }
    if let Some(id) = (request.method == "POST").then(|| project_target(path, "push-target")).flatten() {
        return push_project_target(roots, provider, id, body, confirm.is_some());
    }
    if let Some(item) = (request.method == "POST").then(|| todo_plan_target(path)).flatten() {
        return plan_from_todo(roots, provider, item, body);
    }
    if let Some((id, feature)) = (request.method == "POST").then(|| feature_target(path, PLAN_LEAF)).flatten() {
        return plan_from_draft(roots, provider, id, feature, body);
    }
    if let Some((id, run)) = (request.method == "POST").then(|| run_target(path, "plan-replace")).flatten() {
        return decide_plan(roots, provider, id, run, true, body);
    }
    if let Some((id, run)) = (request.method == "POST").then(|| run_target(path, "plan-discard")).flatten() {
        return decide_plan(roots, provider, id, run, false, body);
    }
    if let Some(target) = (request.method == "POST").then(|| todo_target(path)).flatten() {
        return change_todo(roots, provider, target, body);
    }
    if !matches!(request.method, "GET" | "HEAD") {
        return error(405, "this view is read-only");
    }
    if let Some((id, run)) = run_target(path, "plan") {
        return match provider.map(|provider| provider.plan(id, run)) {
            Some(Ok(Some(plan))) => reply(200, json!({ "plan": plan, "remote_actions": remote_actions_on(roots) })),
            Some(Err(_)) => error(500, "the planning run could not be read"),
            _ => error(404, "not found"),
        };
    }
    if let Some(id) = project_target(path, "push-target") {
        return push_target_state(roots, provider, id);
    }
    if let Some((id, run)) = run_target(path, "resume-check") {
        return resume_check(roots, provider, id, run);
    }
    if let Some((id, run)) = run_target(path, "discard-warning") {
        return discard_warning(roots, provider, id, run);
    }
    if let Some(id) = project_target(path, "queue") {
        // A read: there with actions off, like the project's settings, and its failure names no path;
        // it says whether actions are on, so the page offers its changes only then.
        return match provider.map(|provider| provider.queue(id)) {
            Some(Ok(Some(queue))) => reply(200, json!({ "queue": queue, "remote_actions": remote_actions_on(roots) })),
            Some(Err(_)) => error(500, "the queue could not be read"),
            _ => error(404, "not found"),
        };
    }
    if let Some(id) = project_target(path, "launch-profiles") {
        return launch_profiles(roots, provider, id);
    }
    if let Some(id) = project_target(path, "settings") {
        // A read like /api/settings: there with actions off, and its failure names no path.
        return match provider.map(|provider| provider.project_settings(id)) {
            Some(Ok(Some(settings))) => reply(200, json!({ "settings": settings })),
            Some(Err(_)) => error(500, "the project's settings could not be read"),
            _ => error(404, "not found"),
        };
    }
    if let Some((id, feature)) = feature_target(path, "fields") {
        return feature_fields(roots, provider, id, feature);
    }
    if path == "/api/settings/storage/preview" {
        return storage_preview(roots, provider);
    }
    if path == "/api/storage/worktrees" {
        return kept_worktrees(roots, provider);
    }
    if path == "/api/projects" {
        return match provider {
            Some(provider) => match provider.projects() {
                Ok(projects) => {
                    let mut body = json!({ "projects": projects });
                    if let Some(machine) = provider.machine_runs().ok().flatten() {
                        body["machine"] = machine;
                    }
                    reply(200, body)
                }
                Err(why) => error(500, &why),
            },
            None => error(404, "not found"),
        };
    }
    if path == "/api/todo" {
        return todo_list(roots);
    }
    if path == "/api/stats" {
        return stats(provider, query);
    }
    if path == "/api/audit" {
        return audit(roots, query);
    }
    if path == "/api/settings/sessions" {
        return session_settings(roots);
    }
    if path == "/api/settings/options" {
        return match provider.map(|provider| provider.setting_options()) {
            Some(Ok(Some(options))) => reply(200, json!({ "options": options })),
            Some(Err(_)) => error(500, "the options could not be read"),
            _ => error(404, "not found"),
        };
    }
    if path == "/api/settings" {
        return match provider.map(|provider| (provider.settings(), provider.server())) {
            Some((Ok(Some(settings)), server)) => reply(200, json!({ "settings": settings, "server": server.map(|server| server.status()) })),
            Some((Err(why), _)) => error(500, &why),
            _ => error(404, "not found"),
        };
    }
    if let Some(id) = project_target(path, "features/archived") {
        // A read like the features list: there with actions off, and its failure names no path.
        return match provider.map(|provider| provider.archived_features(id)) {
            Some(Ok(Some(archived))) => reply(200, archived),
            Some(Err(_)) => error(500, "the archive could not be read"),
            _ => error(404, "not found"),
        };
    }
    if let Some(rest) = path.strip_prefix("/api/projects/") {
        if let Some(id) = rest.strip_suffix("/features").filter(|id| !id.is_empty() && !id.contains('/')) {
            return match provider.map(|provider| provider.features(id)) {
                Some(Ok(Some(features))) => {
                    // A reason that cannot be read leaves Start offered: the start itself still checks.
                    let start_blocked = provider.and_then(|provider| provider.start_blocked(id).ok().flatten());
                    // A start while the server waits to restart asks first: the restart then waits for it too.
                    let restart_pending = provider.and_then(|provider| provider.server()).is_some_and(|server| server.restart_pending());
                    reply(200, json!({ "features": features, "remote_actions": remote_actions_on(roots), "start_blocked": start_blocked, "restart_pending": restart_pending }))
                }
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
        if let [id, "runs", run, "design", "image"] = parts.as_slice() {
            let image = query.split('&').find_map(|pair| pair.strip_prefix("path=")).and_then(query_decode).filter(|path| !path.is_empty());
            return match (provider, image) {
                (Some(provider), Some(image)) if !id.is_empty() && !run.is_empty() => match provider.run_design_image(id, run, &image) {
                    Ok(Some(found)) => reply(200, found),
                    // The reason can name a path on this machine, so the caller is only told it failed.
                    Err(_) => error(500, "the image could not be read"),
                    Ok(None) => error(404, "not found"),
                },
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

/// `GET /api/stats?days=7|30|90[&project=<id>]`: how the runs went, for every project or one. A read,
/// so it is there with actions off; its failure names no path.
/// GET /api/audit `?source=web|cli&identity=…&action=…&since=…&until=…`: the newest audit lines that match
/// every filter given; `action` takes a whole action or its kind, and the times are inclusive epoch seconds.
fn audit(roots: &StorageRoots, query: &str) -> Response {
    let mut filter = crate::remote_audit::Filter::default();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let (Some(key), Some(value)) = (query_decode(key), query_decode(value)) else { return error(400, "the query is not valid") };
        if value.is_empty() {
            continue;
        }
        let time = |value: &str| value.parse::<u64>().map_err(|_| error(400, "since and until are seconds since the epoch"));
        match key.as_str() {
            "source" if value == "web" || value == "cli" => filter.source = Some(value),
            "source" => return error(400, "source must be web or cli"),
            "identity" => filter.identity = Some(value),
            "action" => filter.action = Some(value),
            "since" => match time(&value) {
                Ok(since) => filter.since = Some(since),
                Err(refused) => return refused,
            },
            "until" => match time(&value) {
                Ok(until) => filter.until = Some(until),
                Err(refused) => return refused,
            },
            _ => return error(400, "unknown query parameter"),
        }
    }
    match crate::remote_audit::read_filtered(roots, AUDIT_LIMIT, &filter) {
        Ok(lines) => reply(200, json!({ "audit": lines })),
        Err(why) => error(500, &why),
    }
}

fn stats(provider: Option<&dyn ProjectsProvider>, query: &str) -> Response {
    let mut days = 7;
    let mut project = None;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let (Some(key), Some(value)) = (query_decode(key), query_decode(value)) else { return error(400, "the query is not valid") };
        match key.as_str() {
            "days" => match value.as_str() {
                "7" | "30" | "90" => days = value.parse().unwrap_or(7),
                _ => return error(400, "days must be 7, 30 or 90"),
            },
            "project" if !value.is_empty() => project = Some(value),
            _ => return error(400, "unknown query parameter"),
        }
    }
    match provider.map(|provider| provider.stats(project.as_deref(), days)) {
        Some(Ok(Some(stats))) => reply(200, stats),
        Some(Err(_)) => error(500, "the stats could not be read"),
        _ => error(404, "not found"),
    }
}

/// The filter, sort and paging a list endpoint accepts in its query string.
struct ListQuery {
    state: Option<String>,
    oldest: bool,
    limit: Option<usize>,
    cursor: Option<String>,
    /// Only the rows with these ids (`runs=a,b`), as a Stats figure links to the runs it counts.
    only: Option<std::collections::HashSet<String>>,
}

/// One query component with `%XX` escapes and `+` decoded; `None` when an escape is broken or the bytes are not text.
pub fn query_decode(raw: &str) -> Option<String> {
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
        let mut parsed = ListQuery { state: None, oldest: false, limit: None, cursor: None, only: None };
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let (Some(key), Some(value)) = (query_decode(key), query_decode(value)) else { return Err((400, "the query is not valid")) };
            match key.as_str() {
                "state" | "status" => parsed.state = Some(value),
                "runs" => parsed.only = Some(value.split(',').filter(|id| !id.is_empty()).map(str::to_string).collect()),
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
        if let Some(only) = &self.only {
            rows.retain(|row| row[id_key].as_str().is_some_and(|id| only.contains(id)));
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
    /// The commit the run was merged at when it was merged locally, not through a provider review.
    merged_locally: Option<String>,
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
    let unknown = |project, head| Facts { project, head, observed: None, link: None, stale: true, freshness: "unverified", provider_head: None, checked_at: None, merged_locally: None };
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
    let provider_head = match freshness {
        "current" => Some(head.clone()),
        "stale" => lifecycle.observed_head_sha.clone(),
        _ => None,
    };
    let merged_locally = (lifecycle.provider == LOCAL_PROVIDER && lifecycle.state == ProviderState::Merged).then(|| lifecycle.review_id.clone());
    // The review orq recorded is offered while it is current or merely unverified (it exists, the last look
    // just could not confirm it); a review the provider shows on another commit, or a local merge, is not.
    let link = safe_link(&lifecycle.url).filter(|_| freshness != "stale" && merged_locally.is_none());
    Ok(Some(Facts { merged_locally, observed: lifecycle.observed_head_sha, link, stale: lifecycle.stale, freshness, provider_head, checked_at, project, head }))
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
            let merged_locally = f.as_ref().and_then(|f| f.merged_locally.clone());
            let link = f.and_then(|f| f.link);
            let link_allowed = link.is_some();
            json!({
                "generated_at": row.generated_at,
                "run_id": row.run_id, "feature": row.feature, "project": row.project, "current": row.current,
                "status": row.status, "risk": row.risk, "fixed_risk": row.fixed_risk, "stale": row.stale, "link": link,
                "freshness": freshness, "link_allowed": link_allowed, "merged_locally": merged_locally,
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
    let InboxDetail { summary, risks, files, tests, rounds, generated_at, risk_items } = match review_inbox::detail_at(roots, run_id, &f.head) {
        Ok(detail) => detail,
        Err(_) => return error(404, "no review report for this run"),
    };
    let status = review_inbox::status_of_run(roots, run_id).ok().flatten();
    reply(
        200,
        json!({
            "generated_at": generated_at,
            "run_id": run_id, "reported_head_sha": f.head, "observed_head_sha": f.observed, "stale": f.stale, "link": f.link,
            "freshness": f.freshness, "link_allowed": f.link.is_some(), "reviewed_head_sha": f.head, "provider_head_sha": f.provider_head, "checked_at": f.checked_at,
            "summary": summary, "risks": risks, "risk_items": risk_items.iter().map(review_inbox::RiskItem::to_json).collect::<Vec<_>>(), "files": files, "tests": tests, "rounds": rounds,
            "status": status, "merged_locally": f.merged_locally,
        }),
    )
}
