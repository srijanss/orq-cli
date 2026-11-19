//! Draft with Claude: a planning run (a run of kind `plan`) hands a Todo idea, or a draft feature, to an
//! unattended read-only Claude session and acts on the marker it answers with. A Todo idea becomes a draft
//! feature and only then leaves the Todo list; an improved draft waits for the person's Replace or Discard.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use orq_core::remote_inbox::{PlanDecision, PlanIdea, PlanRequest, PlanStart};
use orq_orchestrator::{PlanMarker, RunPhase, RunStore, StoreError};
use serde_json::{json, Value};

use super::{parse_ledger_bytes, read_ledger_bytes, StoreProjects};

/// The event that starts a planning run and tells it apart from a feature run.
pub(super) const PLAN_REQUESTED: &str = "plan.requested";
const PLAN_REPLACED: &str = "plan.replaced";
const PLAN_DISCARDED: &str = "plan.discarded";
/// Where a project keeps its own guidance for drafting, read in place of the default.
const DRAFT_PROMPT_FILE: &str = ".orq/draft_prompt.md";
const MAX_DRAFT_PROMPT_BYTES: u64 = 64 * 1024;
/// Variables the agent needs to reach its account, passed through when serve has them.
const AGENT_ENV: [&str; 2] = ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"];

static PLAN_RUNS: AtomicU64 = AtomicU64::new(0);

/// Whether `events` are a planning run's.
pub(super) fn is_plan(events: &[orq_orchestrator::Event]) -> bool {
    events.iter().any(|event| event.kind == PLAN_REQUESTED)
}

/// The fields of a ledger entry that an improved draft proposes again.
fn draft_fields(entry: &orq_features::FeatureEntry) -> Value {
    json!({
        "name": entry.feature_name,
        "description": entry.description.clone().unwrap_or_default(),
        "test_file": entry.test_file,
        "target_files": entry.target_files,
        "depends_on": entry.depends_on,
    })
}

fn ledger_entries(path: &Path) -> Result<Vec<orq_features::FeatureEntry>, String> {
    match read_ledger_bytes(path)? {
        Some(bytes) => parse_ledger_bytes(bytes),
        None => Ok(Vec::new()),
    }
}

fn is_draft(entry: &orq_features::FeatureEntry) -> bool {
    entry.status == orq_features::LedgerStatus::Known(orq_core::FeatureStatus::Draft)
}

/// Why `marker` cannot be saved as feature `name` in a ledger of `entries`, checked as the feature editor checks.
fn marker_problem(marker: &PlanMarker, name: &str, entries: &[orq_features::FeatureEntry]) -> Option<String> {
    if let Some(problem) = orq_features::feature_name_problem(&marker.name) {
        return Some(format!("the marker's name is not valid: {problem}"));
    }
    if marker.description.trim().is_empty() {
        return Some("the marker has no description".to_string());
    }
    marker.depends_on.iter().find_map(|dependency| {
        if dependency == name {
            Some(format!("{name} cannot depend on itself"))
        } else if !entries.iter().any(|entry| &entry.feature_name == dependency) {
            Some(format!("the marker depends on {dependency}, which is not a feature of this project"))
        } else {
            None
        }
    })
}

/// The project's guidance for drafting: its own file, or orq's default.
fn draft_guidance(root: &Path) -> String {
    use std::io::Read;
    let own = std::fs::File::open(root.join(DRAFT_PROMPT_FILE)).ok().and_then(|file| {
        let mut text = String::new();
        file.take(MAX_DRAFT_PROMPT_BYTES).read_to_string(&mut text).ok()?;
        Some(text).filter(|text| !text.trim().is_empty())
    });
    own.unwrap_or_else(|| orq_orchestrator::DEFAULT_DRAFT_PROMPT.to_string())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

impl StoreProjects {
    /// Lets the planning runs start `program` as their agent instead of `claude`.
    pub fn with_plan_agent(mut self, program: PathBuf) -> Self {
        self.plan_agent = Some(program);
        self
    }

    /// Lets a planning session run for `timeout` instead of [`orq_orchestrator::PLAN_TIMEOUT`].
    pub fn with_plan_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.plan_timeout = Some(timeout);
        self
    }

    pub(super) fn start_plan_run(&self, id: &str, request: &PlanRequest) -> Result<PlanStart, String> {
        let Some((git, root)) = self.settings_key(id)? else { return Ok(PlanStart::Unknown) };
        let ledger = orq_features::ledger_path(&git);
        let entries = ledger_entries(&ledger)?;
        let (feature_id, title, text, current, source) = match &request.idea {
            PlanIdea::Todo { id: item, name, text, .. } => (format!("todo-{item}"), name.clone(), text.clone(), None, "todo"),
            PlanIdea::Draft { feature } => {
                let Some(entry) = entries.iter().find(|entry| &entry.feature_name == feature) else { return Ok(PlanStart::Unknown) };
                if !is_draft(entry) {
                    return Ok(PlanStart::Refused(orq_core::redact::redact(&format!("{feature} is not a draft; only a draft can be improved"))));
                }
                (feature.clone(), feature.clone(), entry.description.clone().unwrap_or_default(), Some(draft_fields(entry)), "draft")
            }
        };
        let agent = self.stored_settings_at(&root)?.settings.implementer.filter(|agent| agent.harness == "claude");
        let current_text = current.as_ref().map(|fields| serde_json::to_string_pretty(fields).unwrap_or_default());
        let idea = orq_orchestrator::PlanIdea { title: &title, text: &text, notes: &request.notes, current: current_text.as_deref() };
        let prompt = orq_orchestrator::plan_prompt(&draft_guidance(&root), &idea);
        let args = orq_orchestrator::plan_agent_args(agent.as_ref().map(|agent| agent.model.as_str()), agent.as_ref().and_then(|agent| agent.effort.as_deref()), &prompt);
        let session = orq_orchestrator::DraftSession {
            program: self.plan_agent.as_ref().map_or_else(|| "claude".to_string(), |program| program.to_string_lossy().into_owned()),
            args,
            env: AGENT_ENV.iter().filter_map(|key| std::env::var(key).ok().map(|value| (key.to_string(), value))).collect(),
            cwd: root.clone(),
        };

        let run_id = format!("plan-{}-{}-{}", now_secs(), std::process::id(), PLAN_RUNS.fetch_add(1, Ordering::Relaxed));
        let record = orq_orchestrator::RunRecord {
            run_id: run_id.clone(),
            project_id: orq_git::project_id(&git),
            feature_id: feature_id.clone(),
            remote: String::new(),
            provider: String::new(),
            target_branch: String::new(),
            base_sha: String::new(),
            branch: String::new(),
            // A planning run has no worktree of its own: it only reads the checkout, so nothing that cleans up
            // a run's worktree may ever be pointed at the checkout. This path is never created.
            worktree: self.roots.state.join("plans").join(&run_id),
            repository: String::new(),
            profiles: Default::default(),
            delivery: Default::default(),
        };
        let store = RunStore::new(&self.roots.state);
        match store.start_run(&record) {
            Ok(()) => {}
            Err(StoreError::FeatureBusy { active_run_id }) => {
                return Ok(PlanStart::Refused(orq_core::redact::redact(&format!("{active_run_id} is already drafting this idea"))));
            }
            Err(error) => return Err(format!("{error:?}")),
        }
        let (todo, todo_version) = match &request.idea {
            PlanIdea::Todo { id, version, .. } => (Some(*id), Some(*version)),
            PlanIdea::Draft { .. } => (None, None),
        };
        let requested = json!({
            "source": source, "todo": todo, "todo_version": todo_version, "title": title,
            "feature": current.as_ref().map(|_| feature_id.clone()), "current": current, "notes": request.notes,
        });
        if let Err(error) = orq_orchestrator::record_plan_event(&store, &run_id, PLAN_REQUESTED, requested) {
            let _ = store.finish_run(&record.project_id, &feature_id, &run_id);
            return Err(error.to_string());
        }

        let roots = self.roots.clone();
        let project = id.to_string();
        let run = run_id.clone();
        let idea = request.idea.clone();
        let timeout = self.plan_timeout.unwrap_or(orq_orchestrator::PLAN_TIMEOUT);
        std::thread::spawn(move || {
            let store = RunStore::new(&roots.state);
            let result = orq_orchestrator::run_plan(&store, &run, &session, timeout, |marker| apply_marker(&roots, &ledger, &idea, marker));
            finished(&roots, &root, &project, &run, &title, &result);
        });
        Ok(PlanStart::Started(run_id))
    }

    /// Planning run `run` of project `id` as the web shows it.
    pub(super) fn plan_of(&self, id: &str, run: &str) -> Result<Option<Value>, String> {
        let Some((_, store)) = self.own_run(id, run)? else { return Ok(None) };
        let events = store.events(run).map_err(|error| format!("{error:?}"))?;
        let Some(requested) = events.iter().find(|event| event.kind == PLAN_REQUESTED).map(|event| event.payload.clone()) else { return Ok(None) };
        let summary = orq_orchestrator::summarize_run(&events);
        let state = match summary.phase {
            RunPhase::Completed => "completed",
            RunPhase::Running => "running",
            _ => "failed",
        };
        let completed = events.iter().rev().find(|event| event.kind == "run.completed").map(|event| event.payload.clone());
        let feature = completed.as_ref().and_then(|payload| payload.get("feature").cloned()).unwrap_or_else(|| requested["feature"].clone());
        let proposal = if state == "completed" { orq_orchestrator::plan_marker(&store, run).map_err(|error| format!("{error:?}"))? } else { None };
        let decision = events.iter().rev().find_map(|event| match event.kind.as_str() {
            PLAN_REPLACED => Some("replaced"),
            PLAN_DISCARDED => Some("discarded"),
            _ => None,
        });
        Ok(Some(redacted(json!({
            "state": state,
            "source": requested["source"],
            "title": requested["title"],
            "feature": feature,
            "reason": if state == "failed" { summary.reason.map(Value::from).unwrap_or(Value::Null) } else { Value::Null },
            "proposal": proposal,
            "current": requested["current"],
            "decision": decision,
        }))))
    }

    /// The person's Replace or Discard of planning run `run`'s improved draft. The run is held (its ending lock)
    /// from reading its choices to recording this one, so of two choices made at once the second sees the first
    /// and is refused; a Replace whose choice cannot be recorded puts the draft back as it was.
    pub(super) fn decide_plan_run(&self, id: &str, run: &str, replace: bool) -> Result<PlanDecision, String> {
        let Some((project, store)) = self.own_run(id, run)? else { return Ok(PlanDecision::Unknown) };
        let _held = orq_orchestrator::run_ending_lock(&store, run)?;
        let events = store.events(run).map_err(|error| format!("{error:?}"))?;
        let Some(requested) = events.iter().find(|event| event.kind == PLAN_REQUESTED).map(|event| event.payload.clone()) else { return Ok(PlanDecision::Unknown) };
        let refused = |why: &str| Ok(PlanDecision::Refused(orq_core::redact::redact(why)));
        if requested["source"] != "draft" {
            return refused("this planning run drafted a Todo idea; there is nothing to replace");
        }
        if orq_orchestrator::summarize_run(&events).phase != RunPhase::Completed {
            return refused("the planning run has no proposal to choose");
        }
        if events.iter().any(|event| event.kind == PLAN_REPLACED || event.kind == PLAN_DISCARDED) {
            return refused("the proposal was already replaced or discarded");
        }
        let feature = requested["feature"].as_str().unwrap_or_default().to_string();
        let mut replaced_in = None;
        if replace {
            let Some(marker) = orq_orchestrator::plan_marker(&store, run).map_err(|error| format!("{error:?}"))? else { return refused("the proposal could not be read") };
            let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
            let ledger = orq_features::ledger_path(&git);
            let before = read_ledger_bytes(&ledger)?;
            if let Some(why) = replace_draft(&ledger, &feature, &requested["current"], &marker)? {
                return refused(&why);
            }
            replaced_in = before.map(|bytes| (ledger, bytes));
        }
        let kind = if replace { PLAN_REPLACED } else { PLAN_DISCARDED };
        if let Err(error) = orq_orchestrator::record_plan_event(&store, run, kind, json!({ "feature": feature })) {
            if let Some((ledger, before)) = replaced_in {
                std::fs::write(&ledger, before).map_err(|put| format!("{error}; and {feature} could not be put back: {put}"))?;
            }
            return Err(error.to_string());
        }
        Ok(PlanDecision::Done)
    }
}

/// Saves `marker`'s text over draft `feature`, when the draft is still as the run read it; why not otherwise.
fn replace_draft(ledger: &Path, feature: &str, read: &Value, marker: &PlanMarker) -> Result<Option<String>, String> {
    use orq_features::EditFieldsError;
    let Some(bytes) = read_ledger_bytes(ledger)? else { return Ok(Some(format!("{feature} is no longer in the ledger"))) };
    let loaded = orq_features::LoadedLedger::from_bytes(ledger, bytes).map_err(|error| format!("{error:?}"))?;
    let Some(entry) = loaded.entries().iter().find(|entry| entry.feature_name == feature) else { return Ok(Some(format!("{feature} is no longer in the ledger"))) };
    if !is_draft(entry) {
        return Ok(Some(format!("{feature} is no longer a draft; nothing was replaced")));
    }
    if &draft_fields(entry) != read {
        return Ok(Some(format!("{feature} changed since Claude read it; nothing was replaced")));
    }
    if let Some(problem) = marker_problem(marker, feature, loaded.entries()) {
        return Ok(Some(problem));
    }
    let edits = orq_features::FieldEdits {
        description: Some(marker.description.clone()),
        depends_on: Some(marker.depends_on.clone()),
        parallelizable: None,
        test_file: Some(marker.test_file.clone().unwrap_or_default()),
        target_files: Some(marker.target_files.clone()),
    };
    match loaded.edit_fields(feature, &edits) {
        Ok(_) => Ok(None),
        Err(EditFieldsError::Invalid(why)) => Ok(Some(why.to_string())),
        Err(EditFieldsError::Save(_)) => Ok(Some("the ledger changed while it was being saved; nothing was replaced".to_string())),
        Err(error) => Err(format!("{error:?}")),
    }
}

/// Checks the marker of a planning run of `idea` and acts on it: a Todo idea is saved as a draft feature and
/// then leaves the Todo list; an improved draft is only checked, and waits for Replace or Discard.
fn apply_marker(roots: &orq_core::storage::StorageRoots, ledger: &Path, idea: &PlanIdea, marker: &PlanMarker) -> Result<Value, String> {
    let bytes = read_ledger_bytes(ledger)?;
    match idea {
        PlanIdea::Draft { feature } => {
            let entries = match bytes {
                Some(bytes) => parse_ledger_bytes(bytes)?,
                None => Vec::new(),
            };
            if let Some(problem) = marker_problem(marker, feature, &entries) {
                return Err(problem);
            }
            Ok(json!({ "feature": feature, "source": "draft" }))
        }
        PlanIdea::Todo { id, version, .. } => {
            let loaded = match bytes {
                Some(bytes) => orq_features::LoadedLedger::from_bytes(ledger, bytes).map_err(|error| format!("the ledger cannot be read: {error:?}"))?,
                None => orq_features::LoadedLedger::absent(ledger),
            };
            if let Some(problem) = marker_problem(marker, &marker.name, loaded.entries()) {
                return Err(problem);
            }
            if loaded.entries().iter().any(|entry| entry.feature_name == marker.name) {
                return Err(format!("the project already has a feature named {}", marker.name));
            }
            let draft = orq_features::FeatureDraft {
                feature_name: marker.name.clone(),
                description: Some(marker.description.clone()),
                depends_on: Some(marker.depends_on.clone()).filter(|names| !names.is_empty()),
                parallelizable: None,
                test_file: marker.test_file.clone().filter(|path| !path.is_empty()),
                target_files: Some(marker.target_files.clone()).filter(|paths| !paths.is_empty()),
            };
            loaded.append(&draft).map_err(|error| format!("the draft could not be saved: {error}"))?;
            // Only once the draft is saved does the item leave the list; one edited meanwhile stays.
            let removed = orq_core::store::Store::open(&roots.database()).and_then(|store| store.todo_delete_at(*id, *version)).unwrap_or(false);
            Ok(json!({ "feature": marker.name, "source": "todo", "todo_removed": removed }))
        }
    }
}

/// Audits and tells about a planning run that ended with `result`.
fn finished(roots: &orq_core::storage::StorageRoots, root: &Path, project: &str, run: &str, title: &str, result: &Result<Value, String>) {
    use orq_core::remote_audit::{record, Attempt, Outcome};
    let (outcome, reason, ending) = match result {
        Ok(payload) => (Outcome::Allowed, format!("{title}: drafted {}", payload["feature"].as_str().unwrap_or_default()), orq_orchestrator::PLAN_ENDING),
        Err(why) => (Outcome::Refused, format!("{title}: {why}"), "failed"),
    };
    let reason = orq_core::redact::redact(&reason);
    let _ = record(roots, &Attempt { identity: "web", action: "plan.finish", project, run: Some(run), outcome, reason: Some(&reason) }, now_secs());
    let Ok(settings) = orq_orchestrator::load_run_settings(root, &|name| std::env::var(name).ok()) else { return };
    let Some(argv) = settings.notify_cmd.filter(|argv| !argv.is_empty()) else { return };
    if result.is_ok() && settings.notify_on.as_deref() == Some("failures") {
        return;
    }
    let detail = result.as_ref().err().cloned().unwrap_or_else(|| format!("draft {}", result.as_ref().map(|payload| payload["feature"].as_str().unwrap_or_default().to_string()).unwrap_or_default()));
    let message = orq_orchestrator::notification_message(title, ending, Some(&detail), None);
    let _ = orq_orchestrator::send_notification_with(&argv, root, &message, std::time::Duration::from_secs(30), settings.notify_env.as_deref().unwrap_or(&[]));
}

/// `value` with every string in it redacted, as the web is shown everything.
fn redacted(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(orq_core::redact::redact(&text)),
        Value::Array(items) => Value::Array(items.into_iter().map(redacted).collect()),
        Value::Object(map) => Value::Object(map.into_iter().map(|(key, value)| (key, redacted(value))).collect()),
        other => other,
    }
}
