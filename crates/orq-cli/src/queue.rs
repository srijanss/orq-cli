//! `orq queue list [--json]|add <feature>...|add --resume <run> [--answer <text>]|add --review-again <run>|
//! add --accept <run>|remove <entry>|pause|resume --repo <path>`: the project's run queue from the
//! terminal, the same queue `orq serve` works and the web's Queue section shows. Nothing here starts
//! a run; the server does.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use orq_core::remote_inbox::QueueChange;
use orq_core::store::run_queue::{QueueAction, QueueKind};
use orq_core::store::Store;

use crate::{fail, printable, usage_error};

pub(crate) fn queue(args: &[OsString]) -> ExitCode {
    let Some(words) = args.iter().map(|arg| arg.to_str()).collect::<Option<Vec<&str>>>() else { return usage_error() };
    let [verb, rest @ .., flag, repo] = words.as_slice() else { return usage_error() };
    if *flag != "--repo" {
        return usage_error();
    }
    let action = match (*verb, rest) {
        ("list", []) => Some(Action::List { json: false }),
        ("list", ["--json"]) => Some(Action::List { json: true }),
        ("add", ["--resume", run, "--answer", answer]) => queued_action(QueueKind::Resume, run, Some(answer)),
        ("add", ["--resume", run]) => queued_action(QueueKind::Resume, run, None),
        ("add", ["--review-again", run]) => queued_action(QueueKind::ReviewAgain, run, None),
        ("add", ["--accept", run]) => queued_action(QueueKind::Accept, run, None),
        ("add", features) if !features.is_empty() && features.iter().all(|f| !f.starts_with('-')) => {
            Some(Action::Change(QueueChange::Add { features: features.iter().map(|f| f.to_string()).collect(), profiles: None }))
        }
        ("remove", [feature]) if !feature.starts_with('-') => Some(Action::Change(QueueChange::Remove(feature.to_string()))),
        ("pause", []) => Some(Action::Change(QueueChange::Pause)),
        ("resume", []) => Some(Action::Change(QueueChange::Resume)),
        _ => None,
    };
    let Some(action) = action else { return usage_error() };
    match run(&action, PathBuf::from(repo)) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(message) => fail(message),
    }
}

/// A Resume (with the person's answer), Review again or Accept of `run`; a run id never reads as an option.
fn queued_action(kind: QueueKind, run: &str, answer: Option<&str>) -> Option<Action> {
    let answer = answer.map(str::trim).filter(|answer| !answer.is_empty()).map(str::to_string);
    (!run.starts_with('-')).then(|| Action::Change(QueueChange::AddAction(QueueAction { kind, run: run.to_string(), answer })))
}

/// How an entry named `name` reads: a run action's `<kind>:<run>` as its label, a feature as itself.
fn shown_entry(name: &str) -> String {
    let action = name.split_once(':').and_then(|(kind, run)| Some(QueueAction { kind: QueueKind::parse(kind)?, run: run.to_string(), answer: None }));
    action.map_or_else(|| name.to_string(), |action| orq_remote::projects::action_label(&action))
}

enum Action {
    List { json: bool },
    Change(QueueChange),
}

fn run(action: &Action, repo: PathBuf) -> Result<String, String> {
    let git = orq_git::resolve_project(&repo).map_err(|error| format!("cannot resolve project: {error:?}"))?;
    let root = orq_git::main_checkout_root(&git).unwrap_or_else(|| git.root.clone());
    let (_, store, runs) = crate::storage::open()?;
    match action {
        Action::List { json } => list(&store, &runs, &git, &root, *json),
        Action::Change(change) => {
            let done = orq_remote::projects::change_queue(&store, &runs, &git, &root, change, "cli")?.map_err(|why| printable(&why))?;
            Ok(match change {
                QueueChange::Add { .. } | QueueChange::AddAction(_) => format!("Queued {}.\n", printable(&done)),
                QueueChange::Remove(_) => format!("Removed {}.\n", printable(&shown_entry(&done))),
                QueueChange::Reorder(_) => format!("Reordered {}.\n", printable(&done)),
                QueueChange::Pause => "The queue is paused.\n".to_string(),
                QueueChange::Resume => "The queue is running.\n".to_string(),
            })
        }
    }
}

fn list(store: &Store, runs: &orq_orchestrator::RunStore, git: &orq_git::Project, root: &std::path::Path, json: bool) -> Result<String, String> {
    let shown = orq_remote::projects::project_queue_json(store, runs, git, root)?;
    if json {
        return Ok(format!("{shown}\n"));
    }
    let mut text = String::new();
    if let Some(reason) = shown["paused"].as_str() {
        text.push_str(&format!("Paused: {}\n", printable(reason)));
    }
    let entries = shown["entries"].as_array().cloned().unwrap_or_default();
    if entries.is_empty() {
        text.push_str("The queue is empty.\n");
    }
    for entry in entries {
        // A run action shows as what it does; a feature's start as the feature, as before.
        let feature = printable(entry["label"].as_str().filter(|_| entry["kind"] != "start").or(entry["feature"].as_str()).unwrap_or_default());
        let state = entry["state"].as_str().unwrap_or_default();
        // A paused entry names its run and why it is paused.
        let detail = match (entry["reason"].as_str(), entry["run"].as_str()) {
            (Some(reason), Some(run)) => format!(" ({}): {}", printable(run), printable(reason)),
            (Some(reason), None) => format!(": {}", printable(reason)),
            (None, Some(run)) => format!(" ({})", printable(run)),
            (None, None) => String::new(),
        };
        text.push_str(&format!("{feature}  {state}{detail}\n"));
    }
    Ok(text)
}
