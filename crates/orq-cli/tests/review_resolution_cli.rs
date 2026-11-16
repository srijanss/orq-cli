//! A review ends without a PR/MR from the terminal too: a run delivered by a local merge resolves its
//! review as merged, and `orq review resolve|dismiss|reopen <run> [--head <sha>]` records what a person
//! decided, in the review index and in the run's events. `orq review backfill` resolves the reviews of
//! runs merged locally before merges resolved them.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OLD: &str = "1111111111111111111111111111111111111111";
const MERGED: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct World {
    repo: GitFixture,
    home: PathBuf,
    ssh: PathBuf,
    path: std::ffi::OsString,
    _provider: FakeProvider,
    _agent: FakeAgent,
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

fn executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

fn world(tag: &str) -> World {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let home = orq_test_support::process_temp_dir().join(format!("orq-review-resolution-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("config/orq")).unwrap();
    let ssh = home.join("ssh");
    executable(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n");
    let provider = FakeProvider::builder().reply(&["pr", "list"], "[]\n").build();
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    executable(&bin.join("claude"), &format!("#!/bin/sh\ncase \"$*\" in *'local gate failed'*) exit 0;; esac\nexec '{}' \"$@\"\n", agent.path().display()));
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(dirs).unwrap();
    World { repo, home, ssh, path, _provider: provider, _agent: agent }
}

fn orq(w: &World, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(args)
        .env("XDG_CONFIG_HOME", w.home.join("config"))
        .env("XDG_DATA_HOME", w.home.join("data"))
        .env("XDG_STATE_HOME", w.home.join("state"))
        .env("XDG_CACHE_HOME", w.home.join("cache"))
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &w.path)
        .env("USER", "dana")
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn roots(w: &World) -> StorageRoots {
    let base = w.home.clone();
    let roots = StorageRoots::resolve(&|name| match name {
        "XDG_DATA_HOME" => Some(base.join("data").display().to_string()),
        "XDG_CONFIG_HOME" => Some(base.join("config").display().to_string()),
        "XDG_STATE_HOME" => Some(base.join("state").display().to_string()),
        "XDG_CACHE_HOME" => Some(base.join("cache").display().to_string()),
        _ => None,
    })
    .unwrap();
    roots.create_all().unwrap();
    roots
}

fn runs(w: &World) -> RunStore {
    RunStore::new(&w.home.join("state/orq"))
}

fn rev(dir: &Path, rev: &str) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(["rev-parse", "--verify", "--quiet", rev]).output().unwrap();
    text(&out.stdout).trim().to_string()
}

#[test]
fn a_run_delivered_by_a_local_merge_resolves_its_review_as_merged() {
    let w = world("merged");
    let repo = w.repo.root().to_str().unwrap().to_string();
    let out = orq(&w, &["run", "--repo", &repo, "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check", "--delivery", "merge-local"]);
    assert!(text(&out.stdout).contains("state: merged"), "{}\n{}", text(&out.stdout), text(&out.stderr));
    let id = runs(&w).run_ids().unwrap().remove(0);
    let head = rev(w.repo.root(), "refs/heads/main");

    let store = Store::open(&roots(&w).database()).unwrap();
    let lifecycles = store.lifecycles_for_run(&id).unwrap();
    let merged = lifecycles.iter().find(|l| l.reported_head_sha == head).unwrap_or_else(|| panic!("the delivered head's review is resolved: {lifecycles:?}"));
    assert_eq!((merged.provider.as_str(), merged.state.as_str(), merged.review_id.as_str()), ("local", "merged", head.as_str()));

    let refused = orq(&w, &["review", "dismiss", &id]);
    assert!(!refused.status.success(), "a merged review cannot be dismissed");
    assert!(text(&refused.stderr).contains("merged"), "{}", text(&refused.stderr));
}

/// A project's run `run-1` reported at OLD and then at HEAD, with its run record in the run store.
fn reported(tag: &str) -> World {
    let w = world(tag);
    let id = orq_git::project_id(&orq_git::resolve_project(w.repo.root()).unwrap());
    let store = Store::open(&roots(&w).database()).unwrap();
    for (run, head) in [("run-1", OLD), ("run-1", HEAD), ("run-2", HEAD)] {
        store.save_review(&roots(&w).data, run, &id, "COM-01", head, br#"{"status":"current","risks":[]}"#).unwrap();
    }
    for run in ["run-1", "run-2"] {
        runs(&w)
            .create(&RunRecord {
                delivery: Default::default(),
                run_id: run.into(),
                project_id: id.clone(),
                feature_id: format!("feat-{run}"),
                remote: String::new(),
                provider: String::new(),
                target_branch: "main".into(),
                base_sha: "c".repeat(40),
                branch: format!("orq/{run}"),
                worktree: w.home.join(run),
                repository: String::new(),
                profiles: Default::default(),
            })
            .unwrap();
    }
    w
}

fn state(w: &World, run: &str) -> (String, String) {
    let store = Store::open(&roots(w).database()).unwrap();
    let all = store.lifecycles_for_run(run).unwrap();
    let latest = all.iter().find(|l| l.reported_head_sha == HEAD).expect("a lifecycle for the latest report");
    (latest.state.as_str().to_string(), latest.gate.as_str().to_string())
}

fn kinds(w: &World, run: &str) -> Vec<(String, serde_json::Value)> {
    runs(w).events(run).unwrap().into_iter().filter(|e| e.kind.starts_with("review.")).map(|e| (e.kind, e.payload)).collect()
}

#[test]
fn a_person_resolves_dismisses_and_reopens_a_review_from_the_terminal() {
    let w = reported("actions");

    let resolved = orq(&w, &["review", "resolve", "run-1", "--head", HEAD]);
    assert!(resolved.status.success(), "{}", text(&resolved.stderr));
    assert_eq!(state(&w, "run-1").1, "resolved");

    let dismissed = orq(&w, &["review", "dismiss", "run-1"]);
    assert!(dismissed.status.success(), "{}", text(&dismissed.stderr));
    assert_eq!(state(&w, "run-1").0, "closed");

    let reopened = orq(&w, &["review", "reopen", "run-1"]);
    assert!(reopened.status.success(), "{}", text(&reopened.stderr));
    assert_eq!(state(&w, "run-1"), ("open".to_string(), "awaiting_human_review".to_string()));

    let by = serde_json::json!({"head_sha": HEAD, "by": "dana"});
    assert_eq!(
        kinds(&w, "run-1"),
        vec![("review.resolved".into(), by.clone()), ("review.dismissed".into(), by.clone()), ("review.reopened".into(), by)],
        "each action is in the run's events, with who took it"
    );
}

#[test]
fn an_action_on_an_older_head_or_an_unknown_run_changes_nothing() {
    let w = reported("refused");

    let stale = orq(&w, &["review", "resolve", "run-1", "--head", OLD]);
    assert!(!stale.status.success());
    assert!(text(&stale.stderr).contains(HEAD), "the newer report is named: {}", text(&stale.stderr));
    let unknown = orq(&w, &["review", "dismiss", "run-9"]);
    assert!(!unknown.status.success());
    let bad = orq(&w, &["review", "resolve", "run-1", "--head", "not-a-sha"]);
    assert!(!bad.status.success());

    assert!(Store::open(&roots(&w).database()).unwrap().lifecycles_for_run("run-1").unwrap().is_empty());
    assert!(kinds(&w, "run-1").is_empty());
}

#[test]
fn backfill_resolves_the_reviews_of_runs_merged_locally_before() {
    let w = reported("backfill");
    let project = runs(&w).load("run-1").unwrap().project_id;
    runs(&w)
        .append_event(&Event {
            schema_version: 1,
            event_id: "merged-1".into(),
            run_id: "run-1".into(),
            project_id: project,
            feature_id: "feat-run-1".into(),
            kind: "merge.completed".into(),
            stage: None,
            attempt: 1,
            occurred_at: "2026-11-01T10:00:00Z".into(),
            payload: serde_json::json!({"head_sha": MERGED, "target_branch": "main"}),
        })
        .unwrap();

    let out = orq(&w, &["review", "backfill"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("1 "), "says how many runs it resolved: {}", text(&out.stdout));
    assert_eq!(state(&w, "run-1").0, "merged");
    assert!(Store::open(&roots(&w).database()).unwrap().lifecycles_for_run("run-2").unwrap().is_empty(), "an unmerged run is left alone");

    let again = orq(&w, &["review", "backfill"]);
    assert!(text(&again.stdout).contains("0 "), "{}", text(&again.stdout));
}
