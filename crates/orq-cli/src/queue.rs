//! `orq queue list [--json]|add <feature>...|remove <feature>|pause|resume --repo <path>`: the
//! project's run queue from the terminal, the same queue `orq serve` works and the web's Queue
//! section shows. Nothing here starts a run; the server does.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use orq_core::remote_inbox::QueueChange;
use orq_core::store::Store;

use crate::{fail, printable, usage_error};

pub(crate) fn queue(args: &[OsString]) -> ExitCode {
    let Some(words) = args.iter().map(|arg| arg.to_str()).collect::<Option<Vec<&str>>>() else { return usage_error() };
    let [verb, rest @ .., flag, repo] = words.as_slice() else { return usage_error() };
    if *flag != "--repo" {
        return usage_error();
    }
    let action = match (*verb, rest) {
        ("list", []) => Action::List { json: false },
        ("list", ["--json"]) => Action::List { json: true },
        ("add", features) if !features.is_empty() && features.iter().all(|f| !f.starts_with('-')) => {
            Action::Change(QueueChange::Add { features: features.iter().map(|f| f.to_string()).collect(), profiles: None })
        }
        ("remove", [feature]) if !feature.starts_with('-') => Action::Change(QueueChange::Remove(feature.to_string())),
        ("pause", []) => Action::Change(QueueChange::Pause),
        ("resume", []) => Action::Change(QueueChange::Resume),
        _ => return usage_error(),
    };
    match run(&action, PathBuf::from(repo)) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(message) => fail(message),
    }
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
            let done = orq_remote::projects::change_queue(&store, &git, &root, change, "cli")?.map_err(|why| printable(&why))?;
            Ok(match change {
                QueueChange::Add { .. } => format!("Queued {}.\n", printable(&done)),
                QueueChange::Remove(_) => format!("Removed {}.\n", printable(&done)),
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
        let feature = printable(entry["feature"].as_str().unwrap_or_default());
        let state = entry["state"].as_str().unwrap_or_default();
        let detail = match (entry["reason"].as_str(), entry["run"].as_str()) {
            (Some(reason), _) => format!(": {}", printable(reason)),
            (None, Some(run)) => format!(" ({})", printable(run)),
            (None, None) => String::new(),
        };
        text.push_str(&format!("{feature}  {state}{detail}\n"));
    }
    Ok(text)
}
