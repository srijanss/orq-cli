use std::collections::BTreeMap;
use std::path::PathBuf;

use orq_core::retention::{plan, Artifact, ArtifactClass, Retained, RetentionPolicy, RunFacts};
use orq_core::run::RunState;

const DAY: u64 = 86_400;
const NOW: u64 = 10_000 * DAY;

fn facts(run: &str, state: RunState, days_ago: u64) -> RunFacts {
    RunFacts { run_id: run.into(), state, ended_at: Some(NOW - days_ago * DAY), review: None, pinned: false, worktree_in_use: false }
}

fn artifact(run: &str, class: ArtifactClass, name: &str, bytes: u64) -> Artifact {
    Artifact { run_id: run.into(), class, rel: PathBuf::from(format!("runs/{run}/{name}")), bytes }
}

fn all(list: Vec<RunFacts>) -> BTreeMap<String, RunFacts> {
    list.into_iter().map(|f| (f.run_id.clone(), f)).collect()
}

#[test]
fn only_artifacts_of_eligible_classes_of_prunable_runs_are_candidates() {
    let runs = all(vec![facts("old", RunState::Failed, 20), facts("new", RunState::Failed, 1)]);
    let items = vec![
        artifact("old", ArtifactClass::Temporary, "tmp/x", 5),
        artifact("old", ArtifactClass::Log, "events.jsonl", 100),
        artifact("old", ArtifactClass::Checkpoint, "handoff.json", 7),
        artifact("old", ArtifactClass::Report, "tests.json", 9),
        artifact("new", ArtifactClass::Log, "events.jsonl", 100),
    ];
    let p = plan(&RetentionPolicy::default(), &runs, &items, NOW);
    let names: Vec<_> = p.candidates.iter().map(|c| c.artifact.rel.to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["runs/old/tmp/x", "runs/old/events.jsonl"]);
    assert_eq!(p.total_bytes(), 105);
    assert!(p.metadata.is_empty());
}

#[test]
fn a_run_kept_for_a_reason_is_listed_with_that_reason() {
    let mut pinned = facts("pinned", RunState::Failed, 500);
    pinned.pinned = true;
    let runs = all(vec![pinned, facts("busy", RunState::Running, 500)]);
    let items = vec![artifact("pinned", ArtifactClass::Log, "events.jsonl", 1), artifact("busy", ArtifactClass::Log, "events.jsonl", 1)];
    let p = plan(&RetentionPolicy::default(), &runs, &items, NOW);
    assert!(p.candidates.is_empty());
    let kept: Vec<_> = p.kept.iter().map(|k| (k.run_id.as_str(), k.why)).collect();
    assert_eq!(kept, [("busy", Retained::Active), ("pinned", Retained::Pinned)]);
}

#[test]
fn files_of_a_run_with_no_record_are_kept() {
    let p = plan(&RetentionPolicy::default(), &BTreeMap::new(), &[artifact("ghost", ArtifactClass::Log, "events.jsonl", 1)], NOW);
    assert!(p.candidates.is_empty());
    assert_eq!(p.kept.len(), 1);
    assert_eq!((p.kept[0].run_id.as_str(), p.kept[0].why), ("ghost", Retained::UnknownRun));
}

#[test]
fn a_run_old_enough_to_lose_its_metadata_is_listed_for_it() {
    let runs = all(vec![facts("ancient", RunState::Failed, 400), facts("middle", RunState::Failed, 100)]);
    let p = plan(&RetentionPolicy::default(), &runs, &[], NOW);
    assert_eq!(p.metadata, ["ancient"]);
}

#[test]
fn the_preview_says_what_how_much_and_why() {
    let runs = all(vec![facts("old", RunState::Failed, 20)]);
    let p = plan(&RetentionPolicy::default(), &runs, &[artifact("old", ArtifactClass::Log, "events.jsonl", 2048)], NOW);
    let text = p.render().join("\n");
    assert!(text.contains("runs/old/events.jsonl"), "{text}");
    assert!(text.contains("2.0 KiB"), "{text}");
    assert!(text.contains("failed") && text.contains("20 days"), "{text}");
}

#[test]
fn an_empty_plan_says_there_is_nothing_to_remove() {
    let p = plan(&RetentionPolicy::default(), &BTreeMap::new(), &[], NOW);
    assert!(p.render().join("\n").contains("Nothing to prune"));
}
