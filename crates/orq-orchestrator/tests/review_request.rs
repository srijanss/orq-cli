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
    world_pinned(tag, provider, target, remote_url, "")
}

/// A world whose run was started against `repository` (empty: not pinned).
fn world_pinned(tag: &str, provider: &str, target: &str, remote_url: &str, repository: &str) -> World {
    let root = orq_test_support::process_temp_dir().join(format!("orq-review-req-{}-{tag}", std::process::id()));
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
            repository: repository.to_string(),
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
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], "Creating pull request\nhttps://github.example/o/r/pull/7\n")
        .reply(&["pr", "view"], "{\"number\":7,\"url\":\"https://github.example/o/r/pull/7\"}\n")
        .build()
}

fn gitlab() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["mr", "list"], "[]\n")
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
    let create = calls.iter().find(|c| c.args[1] == "create").expect("a create call");
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
    let calls = provider.calls();
    let args = calls.iter().find(|c| c.args[1] == "create").unwrap().args.join("\u{1}");
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
    let provider = FakeProvider::builder().reply(&["pr", "list"], "[]\n").fail(&["pr", "create"], 1, "HTTP 401\n").build();

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
        let provider = FakeProvider::builder().reply(&["pr", "list"], "[]\n").reply(&["pr", "create"], "ok\n").reply(&["pr", "view"], view).build();

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
        assert!(matches!(call.args[1].as_str(), "list" | "create" | "view"), "{:?}", call.args);
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
             if [ \"$2\" = list ]; then echo '[]'; fi\n\
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
    let events = format!("{:?}", w.store.events("run-1").unwrap());
    assert!(events.contains("stage.completed"), "the events were really read: {events}");
    for secret in ["tok-gh", "tok-github"] {
        assert!(!events.contains(secret), "a token is never logged: {events}");
    }
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
        assert_eq!(calls.len(), 3, "{tag}");
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

fn failed_without_a_review(w: &World, outcome: &ReviewRequestOutcome, tag: &str) {
    assert!(matches!(outcome, ReviewRequestOutcome::Failed { .. }), "{tag}: {outcome:?}");
    assert_eq!(kinds(w), ["stage.started", "stage.failed"], "{tag}: no false completion");
    assert_eq!(recorded_review(&w.store, "run-1").unwrap(), None, "{tag}");
}

#[test]
fn a_view_that_fails_after_a_successful_create_fails_the_stage_and_records_nothing() {
    let w = world("view-fails", "github", "main");
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], "https://github.example/o/r/pull/7\n")
        .fail(&["pr", "view"], 1, "HTTP 502\n")
        .build();

    let outcome = create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();

    failed_without_a_review(&w, &outcome, "view");
    assert_eq!(provider.calls().len(), 3);
}

#[test]
fn a_provider_cli_that_cannot_start_fails_the_stage() {
    let w = world("no-cli", "github", "main");

    let outcome = create_review_request(&w.store, "run-1", Path::new("/nonexistent/gh"), &draft(), WAIT).unwrap();

    failed_without_a_review(&w, &outcome, "missing");
}

#[test]
fn a_provider_cli_that_never_answers_is_stopped_and_fails_the_stage() {
    use std::os::unix::fs::PermissionsExt;
    let w = world("hang", "github", "main");
    let script = w.worktree.parent().unwrap().join("gh-hang");
    fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let started = std::time::Instant::now();

    let outcome = create_review_request(&w.store, "run-1", &script, &draft(), Duration::from_millis(300)).unwrap();

    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    failed_without_a_review(&w, &outcome, "timeout");
}

fn git_in(w: &World, args: &[&str]) {
    let out = std::process::Command::new("git").arg("-C").arg(&w.worktree).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn the_repository_is_where_the_branch_is_pushed_not_where_it_is_fetched_from() {
    let w = world_with_remote("pushurl", "github", "main", "https://github.example/upstream/r.git");
    git_in(&w, &["remote", "set-url", "--push", "origin", "git@github.example:me/fork.git"]);
    let provider = github();

    create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();

    for call in provider.calls() {
        let at = call.args.iter().position(|a| a == "--repo").unwrap();
        assert_eq!(call.args[at + 1], "https://github.example/me/fork.git");
    }
}

#[test]
fn a_remote_that_pushes_to_several_urls_is_ambiguous_and_nothing_is_called() {
    let w = world("many-push", "github", "main");
    git_in(&w, &["remote", "set-url", "--add", "--push", "origin", "https://github.example/a/one.git"]);
    git_in(&w, &["remote", "set-url", "--add", "--push", "origin", "https://github.example/b/two.git"]);
    let provider = github();

    assert!(create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).is_err());
    assert!(provider.calls().is_empty());
}

#[test]
fn a_remote_url_with_a_query_or_fragment_is_refused_so_no_secret_reaches_events_or_arguments() {
    for (tag, url) in [
        ("query", "https://github.example/o/r.git?access_token=sekrit"),
        ("fragment", "https://github.example/o/r.git#sekrit"),
        ("scp-query", "git@github.example:o/r.git?token=sekrit"),
    ] {
        let w = world_with_remote(tag, "github", "main", url);
        let provider = github();

        assert!(create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).is_err(), "{tag}");

        assert!(provider.calls().is_empty(), "{tag}");
        let events = format!("{:?}", w.store.events("run-1").unwrap());
        assert!(!events.contains("sekrit"), "{tag}: {events}");
    }
}

/// Rows from the run's own repository; `provider` picks the fields that say so.
fn listing(provider: &str, number_key: &str, url_key: &str, items: &[(u64, &str, &str)]) -> String {
    let own = if provider == "glab" { "\"source_project_id\":9,\"target_project_id\":9" } else { "\"isCrossRepository\":false" };
    let rows: Vec<String> = items
        .iter()
        .map(|(n, state, url)| format!("{{\"{number_key}\":{n},\"{url_key}\":\"{url}\",\"state\":\"{state}\",{own}}}"))
        .collect();
    format!("[{}]\n", rows.join(","))
}

fn gh_with_listing(rows: &[(u64, &str, &str)]) -> FakeProvider {
    FakeProvider::builder().reply(&["pr", "list"], &listing("gh", "number", "url", rows)).reply(&["pr", "edit"], "ok\n").build()
}

fn has_call(provider: &FakeProvider, verb: &str) -> bool {
    provider.calls().iter().any(|c| c.args[1] == verb)
}

#[test]
fn an_open_review_for_the_same_branches_is_adopted_and_nothing_is_created() {
    let w = world("adopt-open", "github", "release/1.x");
    let provider = gh_with_listing(&[(5, "OPEN", "https://github.example/o/r/pull/5")]);

    let outcome = create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();

    let adopted = ReviewRef { url: "https://github.example/o/r/pull/5".to_string(), id: "5".to_string() };
    assert_eq!(outcome, ReviewRequestOutcome::Adopted(adopted.clone()));
    assert!(!has_call(&provider, "create"), "no duplicate is created");
    let list = provider.calls().into_iter().find(|c| c.args[1] == "list").unwrap();
    let args = list.args.join("\u{1}");
    assert!(args.contains("--head\u{1}fix--COM-02") && args.contains("--base\u{1}release/1.x") && args.contains("--state\u{1}all"), "{args}");
    assert!(args.contains("--repo\u{1}https://github.example/o/r.git"), "{args}");
    assert_eq!(kinds(&w), ["stage.started", "review.adopted", "review.updated", "stage.completed"]);
    assert_eq!(recorded_review(&w.store, "run-1").unwrap(), Some(adopted));
}

#[test]
fn a_merged_review_is_adopted_too_and_an_open_one_is_preferred() {
    let merged = world("adopt-merged", "github", "main");
    let provider = gh_with_listing(&[(4, "MERGED", "https://github.example/o/r/pull/4")]);
    let outcome = create_review_request(&merged.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();
    assert!(matches!(&outcome, ReviewRequestOutcome::Adopted(r) if r.id == "4"), "{outcome:?}");

    let both = world("adopt-both", "github", "main");
    let provider = gh_with_listing(&[(4, "MERGED", "https://github.example/o/r/pull/4"), (9, "OPEN", "https://github.example/o/r/pull/9")]);
    let outcome = create_review_request(&both.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();
    assert!(matches!(&outcome, ReviewRequestOutcome::Adopted(r) if r.id == "9"), "{outcome:?}");
}

#[test]
fn a_closed_review_is_not_adopted_and_a_new_one_is_created() {
    let w = world("closed", "github", "main");
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], &listing("gh", "number", "url", &[(3, "CLOSED", "https://github.example/o/r/pull/3")]))
        .reply(&["pr", "create"], "ok\n")
        .reply(&["pr", "view"], "{\"number\":8,\"url\":\"https://github.example/o/r/pull/8\"}\n")
        .build();

    let outcome = create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();

    assert!(matches!(&outcome, ReviewRequestOutcome::Created(r) if r.id == "8"), "{outcome:?}");
}

#[test]
fn a_gitlab_merge_request_is_adopted_from_its_listing() {
    let w = world("adopt-glab", "gitlab", "main");
    let rows = listing("glab", "iid", "web_url", &[(2, "opened", "https://gitlab.example/o/r/-/merge_requests/2")]);
    let provider = FakeProvider::builder().reply(&["mr", "list"], &rows).build();

    let outcome = create_review_request(&w.store, "run-1", &provider.glab(), &draft(), WAIT).unwrap();

    assert!(matches!(&outcome, ReviewRequestOutcome::Adopted(r) if r.id == "2"), "{outcome:?}");
    let list = provider.calls().into_iter().find(|c| c.args[1] == "list").unwrap();
    let args = list.args.join("\u{1}");
    assert!(args.contains("--source-branch\u{1}fix--COM-02") && args.contains("--target-branch\u{1}main") && args.contains("--all"), "{args}");
    assert!(!has_call(&provider, "create"));
}

#[test]
fn a_lookup_that_fails_or_is_unusable_fails_the_stage_without_creating() {
    for (tag, provider) in [
        ("list-fails", FakeProvider::builder().fail(&["pr", "list"], 1, "HTTP 500\n").build()),
        ("list-junk", FakeProvider::builder().reply(&["pr", "list"], "not json\n").build()),
        (
            "list-bad-url",
            FakeProvider::builder()
                .reply(&["pr", "list"], &listing("gh", "number", "url", &[(5, "OPEN", "javascript:alert(1)")]))
                .build(),
        ),
    ] {
        let w = world(tag, "github", "main");

        let outcome = create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();

        assert!(matches!(outcome, ReviewRequestOutcome::Failed { .. }), "{tag}: {outcome:?}");
        assert!(!has_call(&provider, "create"), "{tag}: a blind create could duplicate");
        assert_eq!(recorded_review(&w.store, "run-1").unwrap(), None, "{tag}");
    }
}

#[test]
fn a_retry_after_a_create_whose_readback_failed_adopts_the_review_that_exists() {
    let w = world("retry", "github", "main");
    let first = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], "ok\n")
        .fail(&["pr", "view"], 1, "HTTP 502\n")
        .build();
    let failed = create_review_request(&w.store, "run-1", &first.gh(), &draft(), WAIT).unwrap();
    assert!(matches!(failed, ReviewRequestOutcome::Failed { .. }));

    let second = gh_with_listing(&[(7, "OPEN", "https://github.example/o/r/pull/7")]);
    let retried = create_review_request(&w.store, "run-1", &second.gh(), &draft(), WAIT).unwrap();

    assert!(matches!(&retried, ReviewRequestOutcome::Adopted(r) if r.id == "7"), "{retried:?}");
    assert!(!has_call(&second, "create"));
}

#[test]
fn a_listing_row_that_is_not_understood_fails_the_stage_without_creating() {
    let url = "https://github.example/o/r/pull/5";
    for (tag, body) in [
        ("row-null", "[null]\n".to_string()),
        ("row-number", "[3]\n".to_string()),
        ("row-no-state", format!("[{{\"number\":5,\"url\":\"{url}\"}}]\n")),
        ("row-state-not-text", format!("[{{\"number\":5,\"url\":\"{url}\",\"state\":1}}]\n")),
    ] {
        let w = world(tag, "github", "main");
        let provider = FakeProvider::builder().reply(&["pr", "list"], &body).build();

        let outcome = create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();

        assert!(matches!(outcome, ReviewRequestOutcome::Failed { .. }), "{tag}: {outcome:?}");
        assert!(!has_call(&provider, "create"), "{tag}: a blind create could duplicate");
    }
}

#[test]
fn a_review_from_a_fork_with_the_same_branch_name_is_not_adopted() {
    let url = "https://github.example/o/r/pull/5";
    let gitlab_url = "https://gitlab.example/o/r/-/merge_requests/5";
    for (tag, provider_name, rows, list) in [
        (
            "fork-gh",
            "github",
            format!("[{{\"number\":5,\"url\":\"{url}\",\"state\":\"OPEN\",\"isCrossRepository\":true}}]\n"),
            ["pr", "list"],
        ),
        (
            "fork-gl",
            "gitlab",
            format!("[{{\"iid\":5,\"web_url\":\"{gitlab_url}\",\"state\":\"opened\",\"source_project_id\":4,\"target_project_id\":9}}]\n"),
            ["mr", "list"],
        ),
    ] {
        let w = world(tag, provider_name, "main");
        let (create, view, program_view) = if provider_name == "github" {
            (["pr", "create"], ["pr", "view"], "{\"number\":8,\"url\":\"https://github.example/o/r/pull/8\"}\n")
        } else {
            (["mr", "create"], ["mr", "view"], "{\"iid\":8,\"web_url\":\"https://gitlab.example/o/r/-/merge_requests/8\"}\n")
        };
        let provider =
            FakeProvider::builder().reply(&list, &rows).reply(&create, "ok\n").reply(&view, program_view).build();
        let program = if provider_name == "github" { provider.gh() } else { provider.glab() };

        let outcome = create_review_request(&w.store, "run-1", &program, &draft(), WAIT).unwrap();

        assert!(matches!(&outcome, ReviewRequestOutcome::Created(r) if r.id == "8"), "{tag}: {outcome:?}");
    }
}

#[test]
fn a_listing_that_does_not_say_where_the_source_lives_fails_the_stage() {
    let url = "https://github.example/o/r/pull/5";
    let gitlab_url = "https://gitlab.example/o/r/-/merge_requests/5";
    for (tag, provider_name, rows, list) in [
        ("gh-unsaid", "github", format!("[{{\"number\":5,\"url\":\"{url}\",\"state\":\"OPEN\"}}]\n"), ["pr", "list"]),
        (
            "gh-not-bool",
            "github",
            format!("[{{\"number\":5,\"url\":\"{url}\",\"state\":\"OPEN\",\"isCrossRepository\":\"no\"}}]\n"),
            ["pr", "list"],
        ),
        (
            "gl-unsaid",
            "gitlab",
            format!("[{{\"iid\":5,\"web_url\":\"{gitlab_url}\",\"state\":\"opened\",\"source_project_id\":9}}]\n"),
            ["mr", "list"],
        ),
    ] {
        let w = world(tag, provider_name, "main");
        let provider = FakeProvider::builder().reply(&list, &rows).build();
        let program = if provider_name == "github" { provider.gh() } else { provider.glab() };

        let outcome = create_review_request(&w.store, "run-1", &program, &draft(), WAIT).unwrap();

        assert!(matches!(outcome, ReviewRequestOutcome::Failed { .. }), "{tag}: {outcome:?}");
        assert!(!has_call(&provider, "create"), "{tag}");
    }
}

#[test]
fn the_github_listing_asks_whether_each_review_crosses_repositories() {
    let w = world("asks-cross", "github", "main");
    let provider = gh_with_listing(&[]);
    let _ = create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();

    let list = provider.calls().into_iter().find(|c| c.args[1] == "list").unwrap();
    let args = list.args.join("\u{1}");
    assert!(args.contains("isCrossRepository"), "{args}");
}

#[test]
fn a_listing_that_fills_its_page_may_be_incomplete_so_the_stage_fails_without_creating() {
    for (tag, provider_name, number_key, url_key, list, page_flag) in [
        ("full-gh", "github", "number", "url", ["pr", "list"], "--limit"),
        ("full-gl", "gitlab", "iid", "web_url", ["mr", "list"], "--per-page"),
    ] {
        let state = if provider_name == "github" { "CLOSED" } else { "closed" };
        let url = "https://example.test/o/r/pull/1";
        let rows: Vec<(u64, &str, &str)> = (1..=100).map(|n| (n, state, url)).collect();
        let w = world(tag, provider_name, "main");
        let provider = FakeProvider::builder()
            .reply(&list, &listing(if provider_name == "github" { "gh" } else { "glab" }, number_key, url_key, &rows))
            .build();
        let program = if provider_name == "github" { provider.gh() } else { provider.glab() };

        let outcome = create_review_request(&w.store, "run-1", &program, &draft(), WAIT).unwrap();

        assert!(matches!(outcome, ReviewRequestOutcome::Failed { .. }), "{tag}: {outcome:?}");
        assert!(!has_call(&provider, "create"), "{tag}: a page that is full may hide an older review");
        let listed = provider.calls().into_iter().find(|c| c.args[1] == "list").unwrap();
        assert!(listed.args.windows(2).any(|p| p[0] == page_flag && p[1] == "100"), "{tag}: {:?}", listed.args);
    }
}

#[test]
fn a_listing_row_in_an_unknown_state_fails_the_lookup_for_that_reason() {
    let github = "https://github.example/o/r/pull/5";
    let gitlab = "https://gitlab.example/o/r/-/merge_requests/5";
    for (tag, provider_name, list, row) in [
        (
            "unknown-gh",
            "github",
            ["pr", "list"],
            format!("[{{\"number\":5,\"url\":\"{github}\",\"state\":\"DRAFTING\",\"isCrossRepository\":false}}]\n"),
        ),
        (
            "unknown-gl",
            "gitlab",
            ["mr", "list"],
            format!("[{{\"iid\":5,\"web_url\":\"{gitlab}\",\"state\":\"archived\",\"source_project_id\":9,\"target_project_id\":9}}]\n"),
        ),
    ] {
        let w = world(tag, provider_name, "main");
        let provider = FakeProvider::builder().reply(&list, &row).build();
        let program = if provider_name == "github" { provider.gh() } else { provider.glab() };

        let outcome = create_review_request(&w.store, "run-1", &program, &draft(), WAIT).unwrap();

        assert!(matches!(&outcome, ReviewRequestOutcome::Failed { reason } if reason.contains("unknown state")), "{tag}: {outcome:?}");
        assert!(!has_call(&provider, "create"), "{tag}");
    }
}

#[test]
fn a_remote_repointed_since_the_run_started_gets_no_provider_call() {
    let w = world_pinned("repointed", "github", "main", "https://evil.example/o/r.git", "https://github.example/o/r");
    let provider = github();

    let outcome = create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT);

    assert!(outcome.is_err(), "{outcome:?}");
    assert!(provider.calls().is_empty(), "{:?}", provider.calls());
}

fn edit_call(provider: &FakeProvider, verb: &str) -> String {
    provider.calls().into_iter().find(|c| c.args[1] == verb).map(|c| c.args.join("\u{1}")).unwrap_or_default()
}

#[test]
fn an_adopted_open_pull_request_gets_the_current_title_and_body() {
    let w = world("update-gh", "github", "main");
    let provider = gh_with_listing(&[(5, "OPEN", "https://github.example/o/r/pull/5")]);

    create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();

    let args = edit_call(&provider, "edit");
    let d = draft();
    assert!(args.starts_with("pr\u{1}edit\u{1}5\u{1}"), "{args}");
    assert!(args.contains(&format!("--title\u{1}{}", d.title)) && args.contains(&format!("--body\u{1}{}", d.body)), "{args}");
    assert!(args.contains("--repo\u{1}https://github.example/o/r.git"), "{args}");
}

#[test]
fn an_adopted_open_merge_request_gets_the_current_title_and_description() {
    let w = world("update-glab", "gitlab", "main");
    let rows = listing("glab", "iid", "web_url", &[(2, "opened", "https://gitlab.example/o/r/-/merge_requests/2")]);
    let provider = FakeProvider::builder().reply(&["mr", "list"], &rows).reply(&["mr", "update"], "ok\n").build();

    create_review_request(&w.store, "run-1", &provider.glab(), &draft(), WAIT).unwrap();

    let args = edit_call(&provider, "update");
    let d = draft();
    assert!(args.starts_with("mr\u{1}update\u{1}2\u{1}"), "{args}");
    assert!(args.contains(&format!("--title\u{1}{}", d.title)) && args.contains(&format!("--description\u{1}{}", d.body)), "{args}");
    assert!(kinds(&w).contains(&"review.updated".to_string()));
}

#[test]
fn a_merged_review_is_not_edited() {
    let w = world("update-merged", "github", "main");
    let provider = gh_with_listing(&[(4, "MERGED", "https://github.example/o/r/pull/4")]);

    create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();

    assert!(!has_call(&provider, "edit"));
    assert!(!kinds(&w).contains(&"review.updated".to_string()));
}

#[test]
fn a_failed_edit_still_adopts_the_review_and_records_why() {
    let w = world("update-fails", "github", "main");
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], &listing("gh", "number", "url", &[(5, "OPEN", "https://github.example/o/r/pull/5")]))
        .fail(&["pr", "edit"], 1, "HTTP 403\n")
        .build();

    let outcome = create_review_request(&w.store, "run-1", &provider.gh(), &draft(), WAIT).unwrap();

    assert!(matches!(&outcome, ReviewRequestOutcome::Adopted(r) if r.id == "5"), "{outcome:?}");
    assert_eq!(kinds(&w), ["stage.started", "review.adopted", "review.update_failed", "stage.completed"]);
    let event = w.store.events("run-1").unwrap().into_iter().find(|e| e.kind == "review.update_failed").unwrap();
    assert!(event.payload.to_string().contains("HTTP 403"), "{}", event.payload);
}
