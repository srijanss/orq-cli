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

/// An event time `seconds` from now, in the `YYYY-MM-DDTHH:MM:SSZ` form events are written in.
fn at(seconds: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 + seconds;
    let (days, rest) = (now.div_euclid(86_400), now.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3_600, rest % 3_600 / 60, rest % 60)
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
    let history = [("recent", "run.failed", at(-2 * 3_600)), ("old", "run.failed", at(-3 * 86_400)), ("fine", "run.completed", at(-3_600)), ("future", "run.failed", at(2 * 86_400))];
    for (run, kind, at) in &history {
        runs.create(&record(&base, run, &id)).unwrap();
        runs.append_event(&event(run, &id, kind, at)).unwrap();
    }
    let list = StoreProjects::new(roots, scan).projects().unwrap();
    let busy = &list.as_array().unwrap()[0];
    assert_eq!(busy["failed_runs"], 1, "{list}");
    assert_eq!(busy["failed_window_hours"], 24, "{list}");
}

fn event_with(run: &str, project: &str, kind: &str, at: &str, payload: serde_json::Value) -> Event {
    Event { payload, ..event(run, project, kind, at) }
}

#[test]
fn the_window_is_measured_from_the_event_that_failed_the_run() {
    let base = web_deps::private_temp_dir("projects-attention-failure-time");
    let scan = base.join("code");
    let repo = git_repo(&scan, "busy");
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let id = orq_git::project_id(&orq_git::resolve_project(&repo).unwrap());
    let runs = RunStore::new(&roots.state);
    let none = serde_json::json!({});
    let recent = at(-3_600);
    let old = at(-3 * 86_400);
    let inside = at(-(86_400 - 60));
    let outside = at(-(86_400 + 60));
    let histories: Vec<(&str, Vec<(&str, String, serde_json::Value)>)> = vec![
        // Failed long ago; an unrelated event now must not make it recent again.
        ("old-then-kept", vec![("run.failed", old.clone(), none.clone()), ("worktree.kept", recent.clone(), none.clone())]),
        // Failed recently; an unrelated trailing event with a bad time must not hide it.
        ("recent-then-garbage", vec![("run.failed", recent.clone(), none.clone()), ("worktree.kept", "garbage".into(), none.clone())]),
        ("recent-then-garbage-2", vec![("run.failed", recent.clone(), none.clone()), ("worktree.kept", "garbage".into(), none.clone())]),
        ("stage-failed", vec![("stage.failed", recent.clone(), none.clone())]),
        ("ended-failed", vec![("run.completed", recent.clone(), serde_json::json!({"ending": "failed"}))]),
        ("completed", vec![("run.completed", recent.clone(), serde_json::json!({"ending": "review_ready"}))]),
        ("just-inside", vec![("run.failed", inside, none.clone())]),
        ("just-outside", vec![("run.failed", outside, none.clone())]),
        ("bad-time", vec![("run.failed", "garbage".into(), none.clone())]),
    ];
    for (run, history) in &histories {
        runs.create(&record(&base, run, &id)).unwrap();
        for (n, (kind, time, payload)) in history.iter().enumerate() {
            let mut e = event_with(run, &id, kind, time, payload.clone());
            e.event_id = format!("{run}-{n}");
            runs.append_event(&e).unwrap();
        }
    }
    let list = StoreProjects::new(roots, scan).projects().unwrap();
    // both recent-then-garbage runs, stage-failed, ended-failed and just-inside.
    assert_eq!(list.as_array().unwrap()[0]["failed_runs"], 5, "{list}");
}
