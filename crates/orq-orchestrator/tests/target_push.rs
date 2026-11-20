//! Pushing a project's target branch, as the person asks from the web or `orq push`: the checks
//! that must hold before it is offered, the commits it would push, and the push itself. Each test
//! has a project checkout whose `origin` is a github.com address that git rewrites to a bare
//! repository next to it, and a fake `gh` that answers the branch lookup.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{push_target, target_push_state, Event, RunRecord, RunStore, TargetPush, TargetPushOutcome};
use orq_test_support::FakeProvider;

const ORIGIN: &str = "https://github.com/acme/widgets.git";
const REPOSITORY: &str = "https://github.com/acme/widgets";
const LOOKUP: [&str; 2] = ["api", "repos/acme/widgets/branches/main"];

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, name: &str, subject: &str) -> String {
    fs::write(dir.join(name), format!("{subject}\n")).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", subject]);
    git(dir, &["rev-parse", "HEAD"])
}

struct World {
    root: PathBuf,
    repo: PathBuf,
    origin: PathBuf,
    store: RunStore,
    base: String,
}

/// A checkout on `main` level with its origin, then `ahead` commits of its own on `main`.
fn world(tag: &str, ahead: usize) -> World {
    let root = orq_test_support::process_temp_dir().join(format!("orq-target-push-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let origin = root.join("origin.git");
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&root, &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["remote", "add", "origin", ORIGIN]);
    let local = format!("url.file://{}.insteadOf", origin.display());
    git(&repo, &["config", &local, ORIGIN]);
    let base = commit(&repo, "a.txt", "base");
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);
    for n in 0..ahead {
        commit(&repo, &format!("f{n}.txt"), &format!("change {n}"));
    }
    let store = RunStore::new(&root.join("state"));
    World { root, repo, origin, store, base }
}

fn unprotected() -> FakeProvider {
    FakeProvider::builder().reply(&LOOKUP, r#"{"name":"main","protected":false}"#).build()
}

fn request<'a>(w: &'a World, gh: &'a FakeProvider) -> TargetPush<'a> {
    TargetPush {
        root: &w.repo,
        project_id: "widgets",
        target: "main",
        forge: None,
        store: &w.store,
        gh: gh.gh(),
    }
}

#[test]
fn a_target_two_commits_ahead_and_not_protected_is_pushed_and_origin_then_equals_local() {
    let w = world("pushes", 2);
    let gh = unprotected();
    let push = request(&w, &gh);
    let head = git(&w.repo, &["rev-parse", "main"]);

    let state = target_push_state(&push);
    assert_eq!(state.reason, None, "{state:?}");
    assert_eq!((state.count, state.sha.as_deref()), (2, Some(head.as_str())), "{state:?}");

    match push_target(&push, &head) {
        TargetPushOutcome::Pushed { from, to, count, .. } => assert_eq!((from, to, count), (w.base.clone(), head.clone(), 2)),
        other => panic!("expected a push, got {other:?}"),
    }
    assert_eq!(git(&w.origin, &["rev-parse", "refs/heads/main"]), head, "origin now has the local target");
}

#[test]
fn only_the_target_is_pushed_even_when_the_checkout_follows_tags() {
    let w = world("follow-tags", 1);
    git(&w.repo, &["config", "push.followTags", "true"]);
    git(&w.repo, &["tag", "-a", "-m", "release", "v1", "main"]);
    let gh = unprotected();
    let push = request(&w, &gh);
    let head = git(&w.repo, &["rev-parse", "main"]);

    assert!(matches!(push_target(&push, &head), TargetPushOutcome::Pushed { .. }));
    assert_eq!(git(&w.origin, &["rev-parse", "refs/heads/main"]), head);
    assert_eq!(git(&w.origin, &["tag", "--list"]), "", "no tag went with the push");
}

#[test]
fn a_branch_github_reports_protected_is_refused_after_one_rest_lookup_and_never_graphql() {
    let w = world("protected", 2);
    let gh = FakeProvider::builder().reply(&LOOKUP, r#"{"name":"main","protected":true}"#).build();
    let push = request(&w, &gh);
    let head = git(&w.repo, &["rev-parse", "main"]);

    let state = target_push_state(&push);
    assert_eq!(state.reason.as_deref(), Some("main is protected on GitHub: deliver by review instead"), "{state:?}");
    assert_eq!(state.count, 2, "the commits are still listed: {state:?}");
    match push_target(&push, &head) {
        TargetPushOutcome::Refused(reason) => assert_eq!(reason, "main is protected on GitHub: deliver by review instead"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(git(&w.origin, &["rev-parse", "refs/heads/main"]), w.base, "nothing was pushed");
    let calls = gh.calls();
    assert!(!calls.is_empty(), "GitHub was asked");
    for call in calls {
        assert_eq!((call.program.as_str(), call.args.as_slice()), ("gh", &LOOKUP.map(String::from)[..]), "only the REST lookup is made");
    }
}

#[test]
fn a_failed_branch_lookup_is_refused_with_its_reason_and_protection_is_never_assumed_away() {
    let failing = FakeProvider::builder().fail(&LOOKUP, 1, "HTTP 401: Bad credentials").build();
    let garbled = FakeProvider::builder().reply(&LOOKUP, "<html>nope</html>").build();
    for (tag, gh, why) in [("lookup-fails", &failing, "HTTP 401: Bad credentials"), ("lookup-garbled", &garbled, "GitHub's answer was not JSON")] {
        let w = world(tag, 1);
        let push = request(&w, gh);
        let head = git(&w.repo, &["rev-parse", "main"]);
        let expected = format!("GitHub could not say whether main is protected: {why}");

        assert_eq!(target_push_state(&push).reason, Some(expected.clone()), "{tag}");
        assert_eq!(push_target(&push, &head), TargetPushOutcome::Refused(expected), "{tag}");
        assert_eq!(git(&w.origin, &["rev-parse", "refs/heads/main"]), w.base, "{tag}: nothing was pushed");
    }
}

#[test]
fn a_gitlab_project_is_refused_with_deliver_by_mr_review_and_glab_is_never_run() {
    let w = world("gitlab", 1);
    let gitlab = "https://gitlab.com/acme/widgets.git";
    git(&w.repo, &["remote", "set-url", "origin", gitlab]);
    git(&w.repo, &["config", &format!("url.file://{}.insteadOf", w.origin.display()), gitlab]);
    let gh = unprotected();
    let push = request(&w, &gh);
    let head = git(&w.repo, &["rev-parse", "main"]);

    let state = target_push_state(&push);
    assert_eq!((state.provider.as_deref(), state.reason.as_deref()), (Some("gitlab"), Some("GitLab: deliver by MR review")), "{state:?}");
    assert_eq!(push_target(&push, &head), TargetPushOutcome::Refused("GitLab: deliver by MR review".to_string()));
    assert_eq!(git(&w.origin, &["rev-parse", "refs/heads/main"]), w.base, "nothing was pushed");
    assert_eq!(gh.calls(), vec![], "no provider CLI runs for a GitLab project");

    let github = request(&w, &gh);
    let forced = TargetPush { forge: Some("gh"), ..github };
    assert_eq!(target_push_state(&forced).provider.as_deref(), Some("github"), "the project's forge setting wins");
}

/// A merge-local run `id` of the project, pinned to `repository`, that merged at `head` from `base`.
fn merged_run(w: &World, id: &str, repository: &str, base: &str, head: &str) {
    w.store
        .create(&RunRecord {
            delivery: Default::default(),
            run_id: id.to_string(),
            project_id: "widgets".to_string(),
            feature_id: format!("F-{id}"),
            remote: "origin".to_string(),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: base.to_string(),
            branch: format!("fix--{id}"),
            worktree: w.root.join(id),
            repository: repository.to_string(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    w.store
        .append_event(&Event {
            schema_version: 1,
            event_id: format!("{id}-merged"),
            run_id: id.to_string(),
            project_id: "widgets".to_string(),
            feature_id: format!("F-{id}"),
            kind: "merge.completed".to_string(),
            stage: Some("merge".to_string()),
            attempt: 1,
            occurred_at: "2026-11-20T10:00:00Z".to_string(),
            payload: serde_json::json!({ "head_sha": head }),
        })
        .unwrap();
}

#[test]
fn an_origin_url_other_than_the_one_the_projects_runs_use_is_refused() {
    let w = world("other-remote", 1);
    let head = git(&w.repo, &["rev-parse", "main"]);
    merged_run(&w, "run-1", "https://github.com/acme/other", &w.base, &head);
    let gh = unprotected();
    let push = request(&w, &gh);
    let refusal = "origin pushes to https://github.com/acme/widgets.git, not https://github.com/acme/other that this project's runs use";

    assert_eq!(target_push_state(&push).reason.as_deref(), Some(refusal));
    assert_eq!(push_target(&push, &head), TargetPushOutcome::Refused(refusal.to_string()));
    assert_eq!(git(&w.origin, &["rev-parse", "refs/heads/main"]), w.base, "nothing was pushed");

    let w = world("same-remote", 1);
    let head = git(&w.repo, &["rev-parse", "main"]);
    merged_run(&w, "run-1", REPOSITORY, &w.base, &head);
    let push = request(&w, &gh);
    assert_eq!(target_push_state(&push).reason, None, "the runs' own repository is pushed to");
}

#[test]
fn an_origin_that_pushes_to_more_than_one_address_is_refused_and_neither_gets_the_commits() {
    let refusal = "origin pushes to more than one address: give it one push address first";
    // Two push urls, or two urls and no push url: `git push origin` would send to both.
    for (tag, key) in [("two-pushurls", "remote.origin.pushurl"), ("two-urls", "remote.origin.url")] {
        let w = world(tag, 1);
        let mirror = w.root.join("mirror.git");
        git(&w.root, &["clone", "-q", "--bare", w.origin.to_str().unwrap(), mirror.to_str().unwrap()]);
        if key == "remote.origin.pushurl" {
            git(&w.repo, &["config", "--add", key, ORIGIN]);
        }
        git(&w.repo, &["config", "--add", key, &format!("file://{}", mirror.display())]);
        let gh = unprotected();
        let push = request(&w, &gh);
        let head = git(&w.repo, &["rev-parse", "main"]);

        assert_eq!(target_push_state(&push).reason.as_deref(), Some(refusal), "{tag}");
        assert_eq!(push_target(&push, &head), TargetPushOutcome::Refused(refusal.to_string()), "{tag}");
        assert_eq!(git(&w.origin, &["rev-parse", "refs/heads/main"]), w.base, "{tag}: nothing was pushed");
        assert_eq!(git(&mirror, &["rev-parse", "refs/heads/main"]), w.base, "{tag}: nothing was pushed to the other address");
    }
}

#[test]
fn an_origin_that_fetches_from_one_repository_and_pushes_to_another_is_refused() {
    let w = world("split-remote", 1);
    let fork = "https://github.com/acme/fork.git";
    let forked = w.root.join("fork.git");
    git(&w.root, &["clone", "-q", "--bare", w.origin.to_str().unwrap(), forked.to_str().unwrap()]);
    git(&w.repo, &["config", "remote.origin.pushurl", fork]);
    git(&w.repo, &["config", &format!("url.file://{}.insteadOf", forked.display()), fork]);
    let gh = FakeProvider::builder().reply(&["api", "repos/acme/fork/branches/main"], r#"{"name":"main","protected":false}"#).build();
    let push = request(&w, &gh);
    let head = git(&w.repo, &["rev-parse", "main"]);
    let refusal = "origin fetches from https://github.com/acme/widgets.git but pushes to https://github.com/acme/fork.git: the commits shown would not be the ones pushed";

    assert_eq!(target_push_state(&push).reason.as_deref(), Some(refusal));
    assert_eq!(push_target(&push, &head), TargetPushOutcome::Refused(refusal.to_string()));
    assert_eq!(git(&forked, &["rev-parse", "refs/heads/main"]), w.base, "nothing was pushed");

    // The same repository written another way is one repository.
    git(&w.repo, &["config", "remote.origin.pushurl", "https://github.com/acme/widgets"]);
    let gh = unprotected();
    assert_eq!(target_push_state(&request(&w, &gh)).reason, None);
}

#[test]
fn a_fetch_that_writes_much_to_stderr_still_finishes_and_is_not_taken_for_a_timeout() {
    use std::os::unix::fs::PermissionsExt;
    let w = world("noisy-fetch", 1);
    // Origin's upload-pack writes 200 KB of diagnostics, more than a pipe holds, before serving.
    let noisy = w.root.join("noisy-upload-pack");
    fs::write(&noisy, "#!/bin/sh\nhead -c 200000 /dev/zero | tr '\\0' x >&2\nexec git-upload-pack \"$@\"\n").unwrap();
    fs::set_permissions(&noisy, fs::Permissions::from_mode(0o755)).unwrap();
    git(&w.repo, &["config", "remote.origin.uploadpack", noisy.to_str().unwrap()]);
    let gh = unprotected();
    let started = std::time::Instant::now();

    let state = target_push_state(&request(&w, &gh));
    assert_eq!((state.reason.as_deref(), state.count), (None, 1), "{state:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(30), "the fetch was not held up: {:?}", started.elapsed());
}

#[test]
fn a_branch_that_moved_after_confirmation_is_refused() {
    let w = world("moved", 1);
    let gh = unprotected();
    let push = request(&w, &gh);
    let confirmed = target_push_state(&push).sha.unwrap();
    commit(&w.repo, "late.txt", "late");

    assert_eq!(push_target(&push, &confirmed), TargetPushOutcome::Refused("the branch changed: review again".to_string()));
    assert_eq!(git(&w.origin, &["rev-parse", "refs/heads/main"]), w.base, "nothing was pushed");
}

#[test]
fn commits_link_to_the_run_they_came_from_others_are_not_from_orq_and_pushed_runs_get_target_pushed() {
    let w = world("runs", 0);
    merged_run(&w, "run-0", REPOSITORY, &w.base, &w.base);
    let first = commit(&w.repo, "r1.txt", "run work 1");
    let run_head = commit(&w.repo, "r2.txt", "run work 2");
    merged_run(&w, "run-1", REPOSITORY, &w.base, &run_head);
    let by_hand = commit(&w.repo, "hand.txt", "fix typo by hand");
    let gh = unprotected();
    let push = request(&w, &gh);

    let state = target_push_state(&push);
    let listed: Vec<(&str, Option<&str>)> = state.commits.iter().map(|c| (c.sha.as_str(), c.run.as_deref())).collect();
    assert_eq!(listed, vec![(by_hand.as_str(), None), (run_head.as_str(), Some("run-1")), (first.as_str(), Some("run-1"))]);
    assert_eq!(state.commits[0].short, by_hand[..7]);
    assert_eq!(state.commits[0].subject, "fix typo by hand");

    match push_target(&push, &by_hand) {
        TargetPushOutcome::Pushed { runs, .. } => assert_eq!(runs, vec!["run-1".to_string()]),
        other => panic!("expected a push, got {other:?}"),
    }
    let pushed: Vec<Event> = w.store.events("run-1").unwrap().into_iter().filter(|e| e.kind == "target.pushed").collect();
    assert_eq!(pushed.len(), 1, "{pushed:?}");
    assert_eq!(pushed[0].payload, serde_json::json!({ "sha": by_hand, "remote": "origin", "target": "main" }));
    assert!(!w.store.events("run-0").unwrap().iter().any(|e| e.kind == "target.pushed"), "a run with nothing in the push is left alone");
}

#[test]
fn a_push_origin_rejects_after_the_checks_pass_shows_gits_reason_and_records_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let w = world("rejected", 0);
    let run_head = commit(&w.repo, "r1.txt", "run work");
    merged_run(&w, "run-1", REPOSITORY, &w.base, &run_head);
    let hook = w.origin.join("hooks").join("pre-receive");
    fs::write(&hook, "#!/bin/sh\necho 'denied by policy' >&2\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let gh = unprotected();
    let push = request(&w, &gh);

    assert_eq!(target_push_state(&push).reason, None, "the checks pass");
    match push_target(&push, &run_head) {
        TargetPushOutcome::Refused(reason) => assert!(reason.contains("denied by policy"), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(git(&w.origin, &["rev-parse", "refs/heads/main"]), w.base, "origin keeps its tip");
    assert!(!w.store.events("run-1").unwrap().iter().any(|e| e.kind == "target.pushed"), "no push is recorded");
}

#[test]
fn a_target_that_is_not_a_plain_branch_name_is_refused_before_anything_is_fetched_or_asked() {
    let w = world("odd-target", 1);
    let gh = unprotected();
    for target in ["main:refs/heads/x", "-main", "+main", "ma in", "", "refs/../main"] {
        let push = TargetPush { target, ..request(&w, &gh) };
        let reason = target_push_state(&push).reason.unwrap_or_default();
        assert_eq!(reason, format!("{target:?} is not a plain branch name"), "{target:?}");
    }
    assert_eq!(gh.calls(), vec![], "GitHub was not asked");
}

/// Someone else pushes a commit to origin's `main`, which the checkout has not fetched.
fn origin_moves_on(w: &World) -> String {
    let other = w.root.join("other");
    git(&w.root, &["clone", "-q", w.origin.to_str().unwrap(), other.to_str().unwrap()]);
    let sha = commit(&other, "theirs.txt", "theirs");
    git(&other, &["push", "-q", "origin", "main"]);
    sha
}

#[test]
fn origin_ahead_or_diverged_is_refused_and_nothing_is_force_pushed() {
    for (tag, ahead) in [("origin-ahead", 0), ("diverged", 2)] {
        let w = world(tag, ahead);
        let theirs = origin_moves_on(&w);
        let gh = unprotected();
        let push = request(&w, &gh);
        let head = git(&w.repo, &["rev-parse", "main"]);

        let state = target_push_state(&push);
        assert_eq!(state.reason.as_deref(), Some("origin/main has commits you don't have: pull first"), "{tag}: {state:?}");
        match push_target(&push, &head) {
            TargetPushOutcome::Refused(reason) => assert_eq!(reason, "origin/main has commits you don't have: pull first", "{tag}"),
            other => panic!("{tag}: expected a refusal, got {other:?}"),
        }
        assert_eq!(git(&w.origin, &["rev-parse", "refs/heads/main"]), theirs, "{tag}: origin keeps its own commit");
        assert_eq!(git(&w.repo, &["rev-parse", "main"]), head, "{tag}: the local target is left alone");
    }
}
