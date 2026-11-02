use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{push_run_branch, PushOutcome, RunRecord, RunStore};

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

struct World {
    store: RunStore,
    root: PathBuf,
    remote: PathBuf,
    worktree: PathBuf,
    log: PathBuf,
}

/// A bare remote with `main`, and a worktree on `branch` with one commit of its own.
fn world(tag: &str, branch: &str, target: &str, remote_override: Option<&str>) -> World {
    world_pinned(tag, branch, target, remote_override, "")
}

/// A world whose run was started against `repository` (empty: not pinned).
fn world_pinned(tag: &str, branch: &str, target: &str, remote_override: Option<&str>, repository: &str) -> World {
    let root = std::env::temp_dir().join(format!("orq-push-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let remote = root.join("remote.git");
    git(&root, &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
    let worktree = root.join("worktree");
    git(&root, &["clone", "-q", remote.to_str().unwrap(), worktree.to_str().unwrap()]);
    fs::write(worktree.join("a.txt"), "a\n").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-q", "-m", "base"]);
    git(&worktree, &["push", "-q", "origin", "HEAD:main"]);
    let base = git(&worktree, &["rev-parse", "HEAD"]);
    git(&worktree, &["checkout", "-q", "-B", branch]);
    fs::write(worktree.join("mine.txt"), "mine\n").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-q", "-m", "mine"]);

    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            remote: remote_override.map(str::to_string).unwrap_or_else(|| remote.to_str().unwrap().to_string()),
            provider: "github".to_string(),
            target_branch: target.to_string(),
            base_sha: base,
            branch: branch.to_string(),
            worktree: worktree.clone(),
            repository: repository.to_string(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    let log = root.join("push.log");
    World { store, root, remote, worktree, log }
}

fn kinds(w: &World) -> Vec<String> {
    w.store.events("run-1").unwrap().into_iter().map(|e| e.kind).collect()
}

#[test]
fn the_run_branch_is_pushed_and_its_head_sha_recorded() {
    let w = world("push", "fix--COM-02", "main", None);
    let head = git(&w.worktree, &["rev-parse", "HEAD"]);

    let outcome = push_run_branch(&w.store, "run-1", &w.log).unwrap();

    assert_eq!(outcome, PushOutcome::Pushed { head_sha: head.clone() });
    assert_eq!(git(&w.remote, &["rev-parse", "refs/heads/fix--COM-02"]), head);
    assert_eq!(kinds(&w), ["stage.started", "stage.completed"]);
    let done = w.store.events("run-1").unwrap().pop().unwrap();
    assert_eq!(done.stage.as_deref(), Some("push"));
    assert_eq!(done.payload["pushed_sha"], head.as_str());
}

#[test]
fn the_target_branch_on_the_remote_is_never_touched() {
    let w = world("target", "fix--COM-02", "main", None);
    let before = git(&w.remote, &["rev-parse", "refs/heads/main"]);

    push_run_branch(&w.store, "run-1", &w.log).unwrap();

    assert_eq!(git(&w.remote, &["rev-parse", "refs/heads/main"]), before);
}

#[test]
fn a_run_branch_named_like_the_target_is_refused_without_pushing() {
    let w = world("same", "main", "main", None);
    let before = git(&w.remote, &["rev-parse", "refs/heads/main"]);

    let outcome = push_run_branch(&w.store, "run-1", &w.log).unwrap();

    assert!(matches!(outcome, PushOutcome::Failed { .. }), "{outcome:?}");
    assert_eq!(git(&w.remote, &["rev-parse", "refs/heads/main"]), before);
    assert_eq!(kinds(&w), ["stage.failed"]);
}

#[test]
fn a_push_that_git_rejects_fails_the_stage_and_logs_gits_output() {
    let w = world("reject", "fix--COM-02", "main", Some("/nonexistent/remote.git"));

    let outcome = push_run_branch(&w.store, "run-1", &w.log).unwrap();

    assert!(matches!(outcome, PushOutcome::Failed { .. }), "{outcome:?}");
    assert_eq!(kinds(&w), ["stage.started", "stage.failed"]);
    let logged = fs::read_to_string(&w.log).unwrap();
    assert!(logged.contains("nonexistent"), "git's own message is in the log: {logged}");
    let failed = w.store.events("run-1").unwrap().pop().unwrap();
    assert!(failed.payload["log"].as_str().unwrap().ends_with("push.log"));
}

#[test]
fn a_rejected_push_never_overwrites_a_diverged_remote_branch() {
    let w = world("diverged", "fix--COM-02", "main", None);
    // Someone else already pushed a different commit to the same branch name.
    let other = w.root.join("other");
    git(&w.root, &["clone", "-q", w.remote.to_str().unwrap(), other.to_str().unwrap()]);
    git(&other, &["checkout", "-q", "-b", "fix--COM-02"]);
    fs::write(other.join("theirs.txt"), "theirs\n").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "theirs"]);
    git(&other, &["push", "-q", "origin", "fix--COM-02"]);
    let theirs = git(&other, &["rev-parse", "HEAD"]);

    let outcome = push_run_branch(&w.store, "run-1", &w.log).unwrap();

    assert!(matches!(outcome, PushOutcome::Failed { .. }), "{outcome:?}");
    assert_eq!(git(&w.remote, &["rev-parse", "refs/heads/fix--COM-02"]), theirs);
}

#[test]
fn an_unknown_run_is_an_error() {
    let w = world("unknown", "fix--COM-02", "main", None);

    assert!(push_run_branch(&w.store, "ghost", &w.log).is_err());
}

#[test]
fn credentials_in_the_remote_url_never_reach_the_log_or_the_events() {
    let w = world("secret", "fix--COM-02", "main", Some("http://deploy:s3cret-token@127.0.0.1:1/repo.git"));

    let outcome = push_run_branch(&w.store, "run-1", &w.log).unwrap();

    assert!(matches!(outcome, PushOutcome::Failed { .. }), "{outcome:?}");
    let logged = fs::read_to_string(&w.log).unwrap();
    assert!(!logged.contains("s3cret-token"), "{logged}");
    assert!(logged.contains("127.0.0.1"), "the host is still shown: {logged}");
    let events = serde_json::to_string(&w.store.events("run-1").unwrap()).unwrap();
    assert!(!events.contains("s3cret-token"), "{events}");
}

#[test]
fn an_annotated_tag_is_not_published_even_when_push_follow_tags_is_configured() {
    let w = world("tags", "fix--COM-02", "main", None);
    git(&w.worktree, &["config", "push.followTags", "true"]);
    git(&w.worktree, &["tag", "-a", "-m", "unrelated", "v-unrelated"]);

    let outcome = push_run_branch(&w.store, "run-1", &w.log).unwrap();

    assert!(matches!(outcome, PushOutcome::Pushed { .. }), "{outcome:?}");
    assert_eq!(git(&w.remote, &["tag", "--list"]), "", "only the branch is published");
}

#[test]
fn the_push_names_the_recorded_commit_not_the_branch_that_could_move() {
    let w = world("pinned", "fix--COM-02", "main", None);
    let head = git(&w.worktree, &["rev-parse", "HEAD"]);

    push_run_branch(&w.store, "run-1", &w.log).unwrap();

    let logged = fs::read_to_string(&w.log).unwrap();
    assert!(logged.contains(&format!("{head}:refs/heads/fix--COM-02")), "the source is the commit: {logged}");
}

/// The refs on the remote, to show a refused push changed none of them.
fn remote_refs(w: &World) -> String {
    git(&w.remote, &["for-each-ref"])
}

fn assert_refused_without_publishing(w: &World) {
    let before = remote_refs(w);

    assert!(push_run_branch(&w.store, "run-1", &w.log).is_err());

    assert_eq!(remote_refs(w), before, "the remote is untouched");
    assert!(!kinds(w).contains(&"stage.completed".to_string()), "{:?}", kinds(w));
}

#[test]
fn a_worktree_switched_to_another_branch_is_not_pushed() {
    let w = world("switched", "fix--COM-02", "main", None);
    git(&w.worktree, &["checkout", "-q", "-b", "somebody-elses"]);

    assert_refused_without_publishing(&w);
}

#[test]
fn a_detached_head_is_not_pushed() {
    let w = world("detached", "fix--COM-02", "main", None);
    git(&w.worktree, &["checkout", "-q", "--detach"]);

    assert_refused_without_publishing(&w);
}

#[test]
fn an_unborn_run_branch_is_not_pushed() {
    let w = world("unborn", "fix--COM-02", "main", None);
    git(&w.worktree, &["checkout", "-q", "--orphan", "fix--COM-02-new"]);
    git(&w.worktree, &["branch", "-D", "fix--COM-02"]);
    git(&w.worktree, &["symbolic-ref", "HEAD", "refs/heads/fix--COM-02"]);

    assert_refused_without_publishing(&w);
}

#[test]
fn a_password_with_an_apostrophe_or_a_quote_is_still_stripped_from_the_log() {
    let w = world("quotes", "fix--COM-02", "main", Some("http://deploy:sec'ret\"pw@127.0.0.1:1/repo.git"));

    push_run_branch(&w.store, "run-1", &w.log).unwrap();

    let logged = fs::read_to_string(&w.log).unwrap();
    assert!(!logged.contains("sec'ret") && !logged.contains("ret\"pw") && !logged.contains("pw@"), "{logged}");
    assert!(logged.contains("127.0.0.1"), "the host is still shown: {logged}");
}

#[test]
fn a_remote_repointed_since_the_run_started_is_not_pushed_to() {
    let w = world_pinned("repointed", "fix--COM-02", "main", Some("origin"), "https://github.example/o/r");
    git(&w.worktree, &["remote", "set-url", "origin", "https://evil.example/o/r.git"]);

    let outcome = push_run_branch(&w.store, "run-1", &w.log);

    assert!(format!("{outcome:?}").contains("now resolves"), "{outcome:?}");
    assert!(outcome.is_err(), "{outcome:?}");
    assert!(!git(&w.remote, &["branch", "--list"]).contains("fix--COM-02"));
    assert!(!kinds(&w).contains(&"stage.completed".to_string()));
}

/// A local stand-in for ssh that runs the command git asks for, whatever the host: ssh-style
/// remotes then reach the bare repositories of the test. It is wired in through the user-level
/// environment, because a pinned push ignores the worktree's own configuration.
#[cfg(unix)]
fn fake_ssh() {
    use std::os::unix::fs::PermissionsExt;
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let ssh = std::env::temp_dir().join(format!("orq-fake-ssh-{}", std::process::id()));
        fs::write(&ssh, "#!/bin/sh\nshift\nexec sh -c \"$1\"\n").unwrap();
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
        // GIT_SSH_COMMAND is a shell command: quote the path.
        std::env::set_var("GIT_SSH_COMMAND", format!("'{}'", ssh.to_str().unwrap().replace('\'', "'\\''")));
    });
}

#[cfg(unix)]
#[test]
fn a_rewrite_rule_in_the_worktree_cannot_redirect_a_pinned_push() {
    fake_ssh();
    let w = world_pinned("chained", "fix--COM-02", "main", Some("origin"), "");
    let evil = w.root.join("evil.git");
    git(&w.root, &["init", "-q", "--bare", evil.to_str().unwrap()]);
    let remote = w.remote.to_str().unwrap();
    let checked = format!("git@forge.example:{remote}");
    let redirected = format!("git@evil.example:{}", evil.to_str().unwrap());
    // `remote get-url` rewrites the alias to the checked url once; handing that url to git
    // would rewrite it again, to the other repository.
    git(&w.worktree, &["remote", "set-url", "origin", "https://alias.example/r"]);
    git(&w.worktree, &["config", &format!("url.{checked}.insteadOf"), "https://alias.example/r"]);
    git(&w.worktree, &["config", &format!("url.{redirected}.insteadOf"), &checked]);
    let mut record = w.store.load("run-1").unwrap();
    record.repository = format!("https://forge.example/{}", remote.trim_start_matches('/'));
    w.store.create(&RunRecord { run_id: "run-2".to_string(), ..record }).unwrap();
    let head = git(&w.worktree, &["rev-parse", "HEAD"]);

    let outcome = push_run_branch(&w.store, "run-2", &w.log);

    assert!(outcome.is_ok(), "{outcome:?}");
    assert_eq!(git(&w.remote, &["rev-parse", "refs/heads/fix--COM-02"]), head, "the pinned repository got the branch");
    assert!(git(&evil, &["branch", "--list"]).is_empty(), "the redirect target got nothing");
}

#[cfg(unix)]
#[test]
fn a_remote_that_still_resolves_to_the_pinned_repository_is_pushed_to() {
    fake_ssh();
    let w = world_pinned("pinned-ok", "fix--COM-02", "main", Some("origin"), "");
    let url = format!("git@forge.example:{}", w.remote.to_str().unwrap());
    git(&w.worktree, &["remote", "set-url", "origin", &url]);
    let pinned = format!("https://forge.example/{}", w.remote.to_str().unwrap().trim_start_matches('/'));
    let mut record = w.store.load("run-1").unwrap();
    record.repository = pinned;
    w.store.create(&RunRecord { run_id: "run-2".to_string(), ..record }).unwrap();
    let head = git(&w.worktree, &["rev-parse", "HEAD"]);

    let outcome = push_run_branch(&w.store, "run-2", &w.log);

    assert!(outcome.is_ok(), "{outcome:?}");
    assert_eq!(git(&w.remote, &["rev-parse", "refs/heads/fix--COM-02"]), head);
    // The push names the url that was checked, not the remote's name, which git would look up again.
    let logged = fs::read_to_string(&w.log).unwrap();
    let command = logged.lines().find(|l| l.starts_with("$ git ")).unwrap();
    assert!(command.contains("forge.example") && !command.contains(" origin "), "{command}");
}
