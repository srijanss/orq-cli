//! The server counts, per project, the runs that failed inside a stated time window, so the
//! dashboard shows totals the server supplied and never counts anything itself.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::ProjectsProvider;
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    path
}

fn date(offset: &str) -> String {
    let out = Command::new("date").args(["-u", "-d", offset, "+%Y-%m-%dT%H:%M:%SZ"]).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn record(base: &Path, run: &str, project: &str) -> RunRecord {
    RunRecord { run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: format!("orq/{run}"), worktree: base.join(run), repository: String::new(), profiles: Default::default() }
}

fn event(run: &str, project: &str, kind: &str, at: &str) -> Event {
    Event { schema_version: 1, event_id: format!("{run}-{kind}"), run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), kind: kind.into(), stage: None, attempt: 1, occurred_at: at.into(), payload: serde_json::json!({"reason": "x"}) }
}

#[test]
fn failed_runs_are_counted_only_inside_the_stated_window() {
    let base = web_deps::private_temp_dir("projects-attention");
    let scan = base.join("code");
    let repo = git_repo(&scan, "busy");
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let id = orq_git::project_id(&orq_git::resolve_project(&repo).unwrap());
    let runs = RunStore::new(&roots.state);
    let history = [("recent", "run.failed", date("-2 hours")), ("old", "run.failed", date("-3 days")), ("fine", "run.completed", date("-1 hour")), ("future", "run.failed", date("+2 days"))];
    for (run, kind, at) in &history {
        runs.create(&record(&base, run, &id)).unwrap();
        runs.append_event(&event(run, &id, kind, at)).unwrap();
    }
    let list = StoreProjects::new(roots, scan).projects().unwrap();
    let busy = &list.as_array().unwrap()[0];
    assert_eq!(busy["failed_runs"], 1, "{list}");
    assert_eq!(busy["failed_window_hours"], 24, "{list}");
}
