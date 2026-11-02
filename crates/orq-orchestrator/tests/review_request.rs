use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orq_orchestrator::{
    create_review_request, recorded_review, DraftSource, ReviewDraft, ReviewRef, ReviewRequestOutcome, RunRecord, RunStore,
};
use orq_test_support::FakeProvider;

struct World {
    store: RunStore,
    worktree: PathBuf,
}

fn world(tag: &str, provider: &str, target: &str) -> World {
    world_with_remote(tag, provider, target, "https://github.example/o/r.git")
}

fn world_with_remote(tag: &str, provider: &str, target: &str, remote_url: &str) -> World {
    let root = std::env::temp_dir().join(format!("orq-review-req-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let worktree = root.join("worktree");
    fs::create_dir_all(&worktree).unwrap();
    for args in [vec!["init", "-q", "-b", "main"], vec!["remote", "add", "origin", remote_url]] {
        let out = std::process::Command::new("git").arg("-C").arg(&worktree).args(&args).output().unwrap();
        assert!(out.status.success(), "git {args:?}");
    }
    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            remote: "origin".to_string(),
            provider: provider.to_string(),
            target_branch: target.to_string(),
            base_sha: "0".repeat(40),
            branch: "fix--COM-02".to_string(),
            worktree: worktree.clone(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    World { store, worktree }
}

fn draft() -> ReviewDraft {
    ReviewDraft {
        title: "--Add the 'thing'".to_string(),
        body: "- it adds \"the\" thing\n$(not run)".to_string(),
        source: DraftSource::Agent,
    }
}

const WAIT: Duration = Duration::from_secs(10);

fn github() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["pr", "create"], "Creating pull request\nhttps://github.example/o/r/pull/7\n")
        .reply(&["pr", "view"], "{\"number\":7,\"url\":\"https://github.example/o/r/pull/7\"}\n")
        .build()
}

fn gitlab() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["mr", "create"], "https://gitlab.example/o/r/-/merge_requests/3\n")
        .reply(&["mr", "view"], "{\"iid\":3,\"web_url\":\"https://gitlab.example/o/r/-/merge_requests/3\"}\n")
        .build()
}

fn kinds(w: &World) -> Vec<String> {
    w.store.events("run-1").unwrap().into_iter().map(|e| e.kind).collect()
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a.canonicalize().unwrap() == b.canonicalize().unwrap()
}

#[test]
fn a_github_pull_request_is_opened_from_the_run_branch_against_the_stored_target() {
    let w = world("gh", "github", "release/1.x");
    let provider = github();

    let outcome = create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();

    let expected = ReviewRef { url: "https://github.example/o/r/pull/7".to_string(), id: "7".to_string() };
    assert_eq!(outcome, ReviewRequestOutcome::Created(expected.clone()));
    let calls = provider.calls();
    let create = &calls[0];
    assert_eq!(&create.args[..2], ["pr", "create"]);
    assert!(same_dir(&create.cwd, &w.worktree));
    let args = create.args.join("\u{1}");
    for pair in [["--head", "fix--COM-02"], ["--base", "release/1.x"], ["--title", "--Add the 'thing'"]] {
        assert!(args.contains(&pair.join("\u{1}")), "{pair:?} in {:?}", create.args);
    }
    assert!(create.args.contains(&"- it adds \"the\" thing\n$(not run)".to_string()), "the body is one verbatim argument");
    assert_eq!(recorded_review(&w.store, "run-1").unwrap(), Some(expected));
}

#[test]
fn a_gitlab_merge_request_is_opened_the_same_way() {
    let w = world("glab", "gitlab", "main");
    let provider = gitlab();

    let outcome = create_review_request(&w.store, "run-1", &provider.glab(), &draft(), WAIT).unwrap();

    let expected = ReviewRef { url: "https://gitlab.example/o/r/-/merge_requests/3".to_string(), id: "3".to_string() };
    assert_eq!(outcome, ReviewRequestOutcome::Created(expected));
    let args = provider.calls()[0].args.join("\u{1}");
    assert!(args.starts_with("mr\u{1}create"), "{args}");
    assert!(args.contains("--source-branch\u{1}fix--COM-02") && args.contains("--target-branch\u{1}main"), "{args}");
}

#[test]
fn the_url_and_id_are_recorded_in_a_review_created_event() {
    let w = world("event", "github", "main");

    create_review_request(&w.store, "run-1", &github().gh(), &draft(), WAIT).unwrap();

    assert_eq!(kinds(&w), ["stage.started", "review.created", "stage.completed"]);
    let created = w.store.events("run-1").unwrap().remove(1);
    assert_eq!(created.stage.as_deref(), Some("review"));
    assert_eq!(created.payload["url"], "https://github.example/o/r/pull/7");
    assert_eq!(created.payload["id"], "7");
    assert_eq!(created.payload["provider"], "github");
}

#[test]
fn a_provider_that_refuses_fails_the_stage_and_records_no_review() {
    let w = world("refused", "github", "main");
    let provider = FakeProvider::builder().fail(&["pr", "create"], 1, "HTTP 401\n").build();

    let outcome = create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();

    assert!(matches!(outcome, ReviewRequestOutcome::Failed { .. }), "{outcome:?}");
    assert_eq!(kinds(&w), ["stage.started", "stage.failed"]);
    assert_eq!(recorded_review(&w.store, "run-1").unwrap(), None);
}

#[test]
fn answers_that_are_not_a_web_url_and_a_number_are_not_recorded() {
    for (tag, view) in [
        ("junk", "not json\n"),
        ("scheme", "{\"number\":7,\"url\":\"javascript:alert(1)\"}\n"),
        ("id", "{\"number\":\"7; rm -rf\",\"url\":\"https://github.example/o/r/pull/7\"}\n"),
        ("missing", "{}\n"),
    ] {
        let w = world(tag, "github", "main");
        let provider = FakeProvider::builder().reply(&["pr", "create"], "ok\n").reply(&["pr", "view"], view).build();

        let outcome = create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();

        assert!(matches!(outcome, ReviewRequestOutcome::Failed { .. }), "{tag}: {outcome:?}");
        assert_eq!(recorded_review(&w.store, "run-1").unwrap(), None, "{tag}");
    }
}

#[test]
fn the_provider_is_only_ever_asked_to_create_and_view_never_to_merge() {
    let w = world("nomerge", "github", "main");
    let provider = github();

    create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();

    for call in provider.calls() {
        assert!(matches!(call.args[1].as_str(), "create" | "view"), "{:?}", call.args);
        assert!(!call.args.iter().any(|a| a.contains("merge") && !a.contains('/')), "{:?}", call.args);
    }
}

#[test]
fn an_unknown_provider_or_run_is_an_error_and_nothing_is_called() {
    let w = world("unknown", "bitbucket", "main");
    let provider = github();

    assert!(create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).is_err());
    assert!(create_review_request(&w.store, "ghost", &provider.gh(), &draft(), WAIT).is_err());
    assert!(provider.calls().is_empty());
}

#[test]
fn provider_credentials_from_the_environment_reach_the_cli_and_nothing_else_does() {
    use std::os::unix::fs::PermissionsExt;
    let w = world("env", "github", "main");
    let dir = w.worktree.parent().unwrap().to_path_buf();
    let seen = dir.join("seen.txt");
    let script = dir.join("gh-probe");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$GH_TOKEN|$GITHUB_TOKEN|$GH_HOST|$ORQ_UNRELATED_SECRET\" >> {}\n\
             if [ \"$2\" = view ]; then echo '{{\"number\":7,\"url\":\"https://github.example/o/r/pull/7\"}}'; fi\n",
            seen.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("GH_TOKEN", "tok-gh");
    std::env::set_var("GITHUB_TOKEN", "tok-github");
    std::env::set_var("GH_HOST", "github.example");
    std::env::set_var("ORQ_UNRELATED_SECRET", "leak");

    let outcome = create_review_request(&w.store, "run-1", &script, &draft(), WAIT).unwrap();

    assert!(matches!(outcome, ReviewRequestOutcome::Created(_)), "{outcome:?}");
    let lines = fs::read_to_string(&seen).unwrap();
    assert!(!lines.is_empty());
    for line in lines.lines() {
        assert_eq!(line, "tok-gh|tok-github|github.example|", "credentials pass, other variables do not");
    }
    let events = fs::read_to_string(dir.join("state")).ok();
    assert!(events.map_or(true, |e| !e.contains("tok-gh")), "a token is never logged");
}

#[test]
fn both_calls_name_the_stored_remotes_repository_without_its_credentials() {
    for (tag, url, expected) in [
        ("repo-https", "https://user:secret@github.example/o/r.git", "https://github.example/o/r.git"),
        ("repo-scp", "git@github.example:o/r.git", "https://github.example/o/r.git"),
        ("repo-ssh", "ssh://git@github.example:2222/o/r.git", "https://github.example/o/r.git"),
    ] {
        let w = world_with_remote(tag, "github", "main", url);
        let provider = github();

        create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();

        let calls = provider.calls();
        assert_eq!(calls.len(), 2, "{tag}");
        for call in &calls {
            let at = call.args.iter().position(|a| a == "--repo").unwrap_or_else(|| panic!("{tag}: no --repo in {:?}", call.args));
            assert_eq!(call.args[at + 1], expected, "{tag}");
            assert!(!call.args.iter().any(|a| a.contains("secret")), "{tag}: {:?}", call.args);
        }
    }
}

#[test]
fn a_remote_that_cannot_be_resolved_is_an_error_and_nothing_is_called() {
    let w = world("no-remote", "github", "main");
    let out = std::process::Command::new("git").arg("-C").arg(&w.worktree).args(["remote", "remove", "origin"]).output().unwrap();
    assert!(out.status.success());
    let provider = github();

    assert!(create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).is_err());
    assert!(provider.calls().is_empty());
}
