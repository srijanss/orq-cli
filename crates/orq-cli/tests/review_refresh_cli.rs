use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_test_support::{FakeProvider, GitFixture};

const REPO: &str = "https://github.example/o/r";
const OTHER: &str = "2222222222222222222222222222222222222222";

struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let home = orq_test_support::process_temp_dir().join(format!("orq-refresh-cli-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        Sandbox { home }
    }

    fn orq(&self, args: &[&str], provider: Option<&FakeProvider>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
        command.args(args);
        command.env("HOME", &self.home);
        command.env("XDG_DATA_HOME", self.home.join("data"));
        command.env("XDG_STATE_HOME", self.home.join("state"));
        command.env("XDG_CONFIG_HOME", self.home.join("config"));
        command.env("XDG_CACHE_HOME", self.home.join("cache"));
        command.env_remove("HERDR_ACTIVE_PANE_CWD");
        command.env_remove("GITHUB_TOKEN");
        let system = std::env::var("PATH").unwrap_or_default();
        match provider {
            Some(p) => command.env("PATH", format!("{}:{system}", p.bin_dir().display())),
            None => command.env("PATH", system),
        };
        command.output().unwrap()
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git").arg("-C").arg(repo).args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"]).args(args).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A run with a built report; returns the report's head commit.
fn run_with_report(sandbox: &Sandbox, fixture: &GitFixture) -> String {
    run_with_report_in(sandbox, fixture, None)
}

/// As [`run_with_report`], with the run in its own worktree under `.claude/worktrees` that
/// delivery has since removed.
fn run_with_removed_worktree(sandbox: &Sandbox, fixture: &GitFixture) -> String {
    let worktree = fixture.root().join(".claude").join("worktrees").join("fix--COM-02");
    run_with_report_in(sandbox, fixture, Some(&worktree))
}

fn run_with_report_in(sandbox: &Sandbox, fixture: &GitFixture, worktree: Option<&Path>) -> String {
    let base = git(fixture.root(), &["rev-parse", "HEAD"]);
    std::fs::write(fixture.root().join("a.txt"), "one\n").unwrap();
    git(fixture.root(), &["add", "-A"]);
    git(fixture.root(), &["commit", "--quiet", "-m", "add a"]);
    let head = git(fixture.root(), &["rev-parse", "HEAD"]);
    git(fixture.root(), &["remote", "add", "origin", "https://github.example/o/r.git"]);
    if let Some(worktree) = worktree {
        git(fixture.root(), &["worktree", "add", "--quiet", "-b", "fix--COM-02", worktree.to_str().unwrap(), "HEAD"]);
    }

    let store = RunStore::new(&sandbox.home.join("state").join("orq"));
    let record = RunRecord {
        delivery: Default::default(),
        run_id: "run-1".to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "COM-02".to_string(),
        remote: "origin".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: base.clone(),
        branch: "fix--COM-02".to_string(),
        worktree: worktree.unwrap_or(fixture.root()).to_path_buf(),
        repository: REPO.to_string(),
        profiles: BTreeMap::new(),
    };
    store.create(&record).unwrap();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: "e1".to_string(),
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            kind: "review.created".to_string(),
            stage: Some("review_request".to_string()),
            attempt: 1,
            occurred_at: "2026-01-01T00:00:00Z".to_string(),
            payload: serde_json::json!({ "url": "https://github.example/o/r/pull/7", "id": "7", "provider": "github" }),
        })
        .unwrap();
    let built = sandbox.orq(&["review", "build", "run-1", "--repo", fixture.root().to_str().unwrap(), "--feature", "COM-02", "--base", &base], None);
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    if let Some(worktree) = worktree {
        git(fixture.root(), &["worktree", "remove", "--force", worktree.to_str().unwrap()]);
    }
    head
}

fn listing(state: &str, head: &str) -> String {
    format!(r#"[{{"number":7,"url":"https://github.example/o/r/pull/7","state":"{state}","headRefOid":"{head}","isCrossRepository":false}}]"#)
}

fn text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

impl Sandbox {
    /// Records `run-1`'s review as open at `head`, as a look at the provider last showed it.
    fn open_review(&self, head: &str) {
        use orq_core::review_lifecycle::{Observation, ProviderState, ReviewLifecycle};
        let index = orq_core::store::Store::open(&self.roots().database()).unwrap();
        let open = ReviewLifecycle::associate("github", "7", "https://github.example/o/r/pull/7", head);
        index.save_lifecycle("run-1", &open.apply(Ok(Observation { state: ProviderState::Open, head_sha: head.to_string() }))).unwrap();
    }

    /// Runs `orq serve` in `repo` until it has looked at `run-1`'s review with `provider` and saved
    /// what it saw, the review's last look having been made over ten minutes ago.
    fn look(&self, repo: &Path, provider: &FakeProvider) {
        self.age_check(11 * 60);
        let _server = self.serve(repo, provider);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !(provider.calls().len() == 1 && self.checked_just_now()) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(self.checked_just_now(), "the server looked at the review: {:?}", provider.calls());
        assert_eq!(provider.calls().len(), 1, "{:?}", provider.calls());
    }

    fn checked_just_now(&self) -> bool {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let last: Option<i64> = rusqlite::Connection::open(self.roots().database()).unwrap().query_row("SELECT MAX(last_checked) FROM review_lifecycles", [], |row| row.get(0)).unwrap();
        last.is_some_and(|at| at >= now - 60)
    }
}

#[test]
fn a_pushed_review_waits_for_a_person_on_the_reported_commit() {
    let sandbox = Sandbox::new("open");
    let fixture = GitFixture::new();
    let head = run_with_report(&sandbox, &fixture);
    sandbox.open_review(&head);
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing("OPEN", &head)).build();

    sandbox.look(fixture.root(), &provider);

    let shown = text(&sandbox.orq(&["review", "show", "run-1"], None));
    assert!(shown.contains("awaiting_human_review") && shown.contains("pull/7"), "{shown}");
    assert!(shown.contains(&head) && !shown.contains("stale"), "{shown}");
}

#[test]
fn a_review_merged_by_a_person_resolves_the_gate_and_orq_never_merges() {
    let sandbox = Sandbox::new("merged");
    let fixture = GitFixture::new();
    let head = run_with_report(&sandbox, &fixture);
    sandbox.open_review(&head);
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing("MERGED", &head)).build();

    sandbox.look(fixture.root(), &provider);

    let shown = text(&sandbox.orq(&["review", "show", "run-1"], None));
    assert!(shown.contains("merged") && shown.contains("resolved"), "{shown}");
    for call in provider.calls() {
        assert!(call.args.iter().all(|a| a != "merge" && a != "approve" && a != "create" && a != "close"), "{:?}", call.args);
    }
}

#[test]
fn a_force_pushed_head_marks_the_report_stale() {
    let sandbox = Sandbox::new("stale");
    let fixture = GitFixture::new();
    let head = run_with_report(&sandbox, &fixture);
    sandbox.open_review(&head);
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing("OPEN", OTHER)).build();
    sandbox.look(fixture.root(), &provider);
    let shown = text(&sandbox.orq(&["review", "show", "run-1"], None));
    assert!(shown.contains("stale") && shown.contains(OTHER), "{shown}");
}

#[test]
fn closed_without_merging_is_reported_as_closed() {
    let sandbox = Sandbox::new("closed");
    let fixture = GitFixture::new();
    let head = run_with_report(&sandbox, &fixture);
    sandbox.open_review(&head);
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing("CLOSED", &head)).build();
    sandbox.look(fixture.root(), &provider);
    let shown = text(&sandbox.orq(&["review", "show", "run-1"], None));
    assert!(shown.contains("closed") && !shown.contains("merged"), "{shown}");
}

#[test]
fn an_unreachable_or_refusing_provider_keeps_the_last_known_state_and_says_why() {
    let sandbox = Sandbox::new("errors");
    let fixture = GitFixture::new();
    let head = run_with_report(&sandbox, &fixture);
    sandbox.open_review(&head);
    let good = FakeProvider::builder().reply(&["pr", "list"], &listing("OPEN", &head)).build();
    sandbox.look(fixture.root(), &good);

    for (stderr, word) in [("HTTP 403: Forbidden\n", "permission"), ("HTTP 502: Bad Gateway\n", "could not be reached")] {
        let bad = FakeProvider::builder().fail(&["pr", "list"], 1, stderr).build();
        sandbox.look(fixture.root(), &bad);
        let shown = text(&sandbox.orq(&["review", "show", "run-1"], None));
        assert!(shown.contains("awaiting_human_review") && shown.to_lowercase().contains(word), "{shown}");
    }
}

#[test]
fn a_look_after_the_worktree_is_removed_asks_about_the_pinned_repository_and_keeps_the_state() {
    let sandbox = Sandbox::new("noworktree");
    let fixture = GitFixture::new();
    let head = run_with_removed_worktree(&sandbox, &fixture);
    sandbox.open_review(&head);
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing("MERGED", &head)).build();

    sandbox.look(fixture.root(), &provider);

    let calls = provider.calls();
    let repo = calls[0].args.iter().position(|a| a == "--repo").map(|i| calls[0].args[i + 1].clone());
    assert_eq!(repo.as_deref().map(|r| r.trim_end_matches(".git")), Some(REPO), "{:?}", calls[0].args);
    assert!(calls[0].cwd.exists(), "the provider runs in a directory that exists: {:?}", calls[0].cwd);
    let shown = text(&sandbox.orq(&["review", "show", "run-1"], None));
    assert!(shown.contains("merged") && shown.contains("pull/7"), "{shown}");
}

impl Sandbox {
    fn roots(&self) -> orq_core::storage::StorageRoots {
        let home = self.home.clone();
        orq_core::storage::StorageRoots::resolve(&|name| match name {
            "HOME" => Some(home.display().to_string()),
            "XDG_DATA_HOME" => Some(home.join("data").display().to_string()),
            "XDG_STATE_HOME" => Some(home.join("state").display().to_string()),
            "XDG_CONFIG_HOME" => Some(home.join("config").display().to_string()),
            "XDG_CACHE_HOME" => Some(home.join("cache").display().to_string()),
            _ => None,
        })
        .unwrap()
    }

    /// `GET path` of the review API, as the server answers it.
    fn api(&self, path: &str) -> serde_json::Value {
        let config = orq_core::remote_inbox::RemoteConfig { token: "tok".into() };
        let request = orq_core::remote_inbox::Request { method: "GET", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
        let reply = orq_core::remote_inbox::handle(&self.roots(), &config, "repo-1", "repo-1", &request);
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.body
    }
}

#[test]
fn a_review_orq_created_keeps_its_link_when_the_provider_cannot_be_asked() {
    let sandbox = Sandbox::new("unverified-link");
    let fixture = GitFixture::new();
    let head = run_with_report(&sandbox, &fixture);
    sandbox.open_review(&head);
    let provider = FakeProvider::builder().fail(&["pr", "list"], 1, "HTTP 502: Bad Gateway\n").build();
    sandbox.look(fixture.root(), &provider);

    let detail = sandbox.api("/api/reviews/run-1");
    let row = sandbox.api("/api/reviews")["reviews"][0].clone();
    for body in [&detail, &row] {
        assert_eq!(body["freshness"], "unverified", "{body}");
        assert_eq!(body["link_allowed"], true, "{body}");
        assert_eq!(body["link"], "https://github.example/o/r/pull/7", "{body}");
    }
}

#[test]
fn backfill_records_the_review_a_run_created_as_unverified_once() {
    let sandbox = Sandbox::new("backfill");
    let fixture = GitFixture::new();
    let head = run_with_report(&sandbox, &fixture);

    let first = sandbox.orq(&["review", "backfill"], None);
    assert!(first.status.success(), "{}", text(&first));
    assert!(text(&first).contains("1 review request(s) recorded"), "{}", text(&first));
    let detail = sandbox.api("/api/reviews/run-1");
    assert_eq!((&detail["freshness"], &detail["link_allowed"], &detail["link"]), (&serde_json::json!("unverified"), &serde_json::json!(true), &serde_json::json!("https://github.example/o/r/pull/7")), "{detail}");
    let shown = text(&sandbox.orq(&["review", "show", "run-1"], None));
    assert!(shown.contains("Provider review 7 (github)") && shown.contains(&head), "{shown}");

    let again = sandbox.orq(&["review", "backfill"], None);
    assert!(text(&again).contains("0 review request(s) recorded"), "{}", text(&again));
}

/// `orq run` of COM-01 delivered end to end in `sandbox`: a fake agent commits, the branch is
/// pushed to a bare remote and `provider` opens the pull request. Returns the pushed head.
fn deliver(sandbox: &Sandbox, fixture: &GitFixture, provider: &FakeProvider) -> String {
    use orq_test_support::{write_ledger, FakeAgent, LedgerEntry};
    let remote = fixture.add_bare_remote();
    git(fixture.root(), &["branch", "-M", "main"]);
    git(fixture.root(), &["push", "--quiet", "origin", "main"]);
    git(fixture.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    write_ledger(fixture.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let bin = sandbox.home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let executable = |path: &Path, body: String| {
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    };
    let ssh = bin.join("ssh-shim");
    executable(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n".into());
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let quoted = agent.path().display().to_string().replace('\'', "'\\''");
    executable(&bin.join("claude"), format!("#!/bin/sh\nexec '{quoted}' \"$@\"\n"));
    std::fs::create_dir_all(sandbox.home.join("config/orq")).unwrap();

    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(fixture.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("HOME", &sandbox.home)
        .env("XDG_DATA_HOME", sandbox.home.join("data"))
        .env("XDG_STATE_HOME", sandbox.home.join("state"))
        .env("XDG_CONFIG_HOME", sandbox.home.join("config"))
        .env("XDG_CACHE_HOME", sandbox.home.join("cache"))
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    git(&remote, &["rev-parse", "fix--COM-01"])
}

#[test]
fn a_delivered_pull_request_is_recorded_so_the_review_page_links_it() {
    let sandbox = Sandbox::new("delivered");
    let fixture = GitFixture::new();
    let url = "https://github.example/o/r/pull/7";
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{url}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{url}\"}}\n"))
        .build();
    let head = deliver(&sandbox, &fixture, &provider);

    let index = orq_core::store::Store::open(&sandbox.roots().database()).unwrap();
    let report = index.reviews_for_head_sha(&head).unwrap().pop().expect("the delivered run has a report");
    let lifecycles = index.lifecycles_for_run(&report.run_id).unwrap();
    assert_eq!(lifecycles.len(), 1, "{lifecycles:?}");
    assert_eq!((lifecycles[0].review_id.as_str(), lifecycles[0].url.as_str(), lifecycles[0].reported_head_sha.as_str(), lifecycles[0].observed), ("7", url, head.as_str(), true));

    let config = orq_core::remote_inbox::RemoteConfig { token: "tok".into() };
    let detail = format!("/api/reviews/{}", report.run_id);
    for path in ["/api/reviews", detail.as_str()] {
        let request = orq_core::remote_inbox::Request { method: "GET", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
        let reply = orq_core::remote_inbox::handle(&sandbox.roots(), &config, &report.project, &report.project, &request);
        assert_eq!(reply.status, 200, "{}", reply.body);
        let body = if path == "/api/reviews" { reply.body["reviews"][0].clone() } else { reply.body };
        assert_eq!((&body["freshness"], &body["link_allowed"], &body["link"]), (&serde_json::json!("current"), &serde_json::json!(true), &serde_json::json!(url)), "{body}");
    }
}

/// `orq serve` in `repo` with `provider`'s CLIs first on PATH; killed when dropped.
struct Serving(std::process::Child);

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Sandbox {
    fn serve(&self, repo: &Path, provider: &FakeProvider) -> Serving {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
        command.args(["serve", "--listen", "127.0.0.1:0"]).current_dir(repo);
        command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        for (name, dir) in [("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"), ("XDG_CONFIG_HOME", "config"), ("XDG_CACHE_HOME", "cache")] {
            command.env(name, self.home.join(dir));
        }
        command.env("HOME", &self.home).env_remove("HERDR_ACTIVE_PANE_CWD").env_remove("GITHUB_TOKEN");
        command.env("PATH", format!("{}:{}", provider.bin_dir().display(), std::env::var("PATH").unwrap_or_default()));
        Serving(command.spawn().unwrap())
    }

    /// Makes the run's stored provider check `seconds` old.
    fn age_check(&self, seconds: i64) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        rusqlite::Connection::open(self.roots().database()).unwrap().execute("UPDATE review_lifecycles SET last_checked = ?1", [now - seconds]).unwrap();
    }
}

/// Waits up to 20 s for `provider` to have been called at least `count` times.
fn wait_for_calls(provider: &FakeProvider, count: usize) -> usize {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while provider.calls().len() < count && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    provider.calls().len()
}

#[test]
fn the_server_looks_again_at_an_open_review_checked_over_ten_minutes_ago_and_keeps_what_it_shows() {
    let sandbox = Sandbox::new("serve-refresh");
    let fixture = GitFixture::new();
    let head = run_with_report(&sandbox, &fixture);
    let index = orq_core::store::Store::open(&sandbox.roots().database()).unwrap();
    let open = orq_core::review_lifecycle::ReviewLifecycle::associate("github", "7", "https://github.example/o/r/pull/7", &head);
    index.save_lifecycle("run-1", &open.apply(Ok(orq_core::review_lifecycle::Observation { state: orq_core::review_lifecycle::ProviderState::Open, head_sha: head.clone() }))).unwrap();
    sandbox.age_check(11 * 60);
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing("MERGED", &head)).build();

    let _server = sandbox.serve(fixture.root(), &provider);

    assert_eq!(wait_for_calls(&provider, 1), 1, "{:?}", provider.calls());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while sandbox.api("/api/reviews/run-1")["status"] != "merged" && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(sandbox.api("/api/reviews/run-1")["status"], "merged");
    // Merged is final: the next rounds leave it alone.
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert_eq!(provider.calls().len(), 1, "{:?}", provider.calls());
}

#[test]
fn a_local_merge_recorded_while_the_provider_is_asked_is_kept() {
    let sandbox = Sandbox::new("merge-during-look");
    let fixture = GitFixture::new();
    let head = run_with_report(&sandbox, &fixture);
    sandbox.open_review(&head);
    sandbox.age_check(11 * 60);
    let provider = FakeProvider::builder().reply_slowly(&["pr", "list"], &listing("OPEN", &head), 1500).build();
    let merged = orq_core::review_lifecycle::ReviewLifecycle::merged_locally(&head, &"9".repeat(40));

    let server = sandbox.serve(fixture.root(), &provider);
    assert_eq!(wait_for_calls(&provider, 1), 1, "the look is under way: {:?}", provider.calls());
    orq_core::store::Store::open(&sandbox.roots().database()).unwrap().save_lifecycle("run-1", &merged).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(3));
    drop(server);

    assert_eq!(provider.calls().len(), 1, "{:?}", provider.calls());
    let kept = orq_core::store::Store::open(&sandbox.roots().database()).unwrap().lifecycles_for_run("run-1").unwrap();
    assert_eq!(kept, vec![merged], "the look saved nothing over the local merge");
    assert_eq!(sandbox.api("/api/reviews/run-1")["link"], serde_json::Value::Null);
}

#[test]
fn a_run_merged_locally_is_not_looked_at_again() {
    let sandbox = Sandbox::new("merged-here");
    let fixture = GitFixture::new();
    let head = run_with_report(&sandbox, &fixture);
    sandbox.open_review(&head);
    sandbox.age_check(11 * 60);
    let merged = orq_core::review_lifecycle::ReviewLifecycle::merged_locally(&head, &"9".repeat(40));
    orq_core::store::Store::open(&sandbox.roots().database()).unwrap().save_lifecycle("run-1", &merged).unwrap();
    let before = orq_core::store::Store::open(&sandbox.roots().database()).unwrap().lifecycles_for_run("run-1").unwrap();
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing("OPEN", &head)).build();

    let server = sandbox.serve(fixture.root(), &provider);
    std::thread::sleep(std::time::Duration::from_secs(3));
    drop(server);
    assert!(provider.calls().is_empty(), "{:?}", provider.calls());
    assert_eq!(orq_core::store::Store::open(&sandbox.roots().database()).unwrap().lifecycles_for_run("run-1").unwrap(), before);
}
