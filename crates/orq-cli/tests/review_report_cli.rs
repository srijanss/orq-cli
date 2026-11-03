use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::GitFixture;

struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let home = orq_test_support::process_temp_dir().join(format!("orq-review-cli-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        Sandbox { home }
    }

    fn data(&self) -> PathBuf {
        self.home.join("data").join("orq")
    }

    fn orq(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
        command.args(args);
        command.env("HOME", &self.home);
        command.env("XDG_DATA_HOME", self.home.join("data"));
        command.env("XDG_STATE_HOME", self.home.join("state"));
        command.env("XDG_CONFIG_HOME", self.home.join("config"));
        command.env("XDG_CACHE_HOME", self.home.join("cache"));
        command.env_remove("HERDR_ACTIVE_PANE_CWD");
        command.env_remove("GITHUB_TOKEN");
        command.output().unwrap()
    }

    fn write_run_file(&self, run: &str, name: &str, body: &str) -> PathBuf {
        let dir = self.data().join("runs").join(run);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn commit(repo: &Path, message: &str) -> String {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "--quiet", "-m", message]);
    git(repo, &["rev-parse", "HEAD"])
}

fn build(sandbox: &Sandbox, run: &str, repo: &Path, base: &str, extra: &[&str]) -> Output {
    let mut args = vec!["review", "build", run, "--repo", repo.to_str().unwrap(), "--feature", "feat", "--base", base];
    args.extend_from_slice(extra);
    sandbox.orq(&args)
}

fn show(sandbox: &Sandbox, run: &str, extra: &[&str]) -> (bool, String, String) {
    let mut args = vec!["review", "show", run];
    args.extend_from_slice(extra);
    let output = sandbox.orq(&args);
    (output.status.success(), String::from_utf8_lossy(&output.stdout).into_owned(), String::from_utf8_lossy(&output.stderr).into_owned())
}

fn passing(head: &str) -> String {
    format!(r#"{{"schema_version":1,"head_sha":"{head}","commands":[{{"command":"cargo test","exit_code":0}}]}}"#)
}

#[test]
fn a_built_report_shows_the_final_commit_and_test_status_without_findings() {
    let sandbox = Sandbox::new("full");
    let fixture = GitFixture::new();
    let base = git(fixture.root(), &["rev-parse", "HEAD"]);
    std::fs::write(fixture.root().join("a.txt"), "one\ntwo\n").unwrap();
    let head = commit(fixture.root(), "add a");
    sandbox.write_run_file("run-1", "tests.json", &passing(&head));
    let review = sandbox.write_run_file("run-1", "review-input.json", &format!(r#"{{"schema_version":1,"head_sha":"{head}","rounds":[{{}},{{}}],"outcome":"approved"}}"#));

    let built = build(&sandbox, "run-1", fixture.root(), &base, &["--review-input", review.to_str().unwrap()]);
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));

    let (ok, text, err) = show(&sandbox, "run-1", &[]);
    assert!(ok, "{err}");
    assert!(text.contains(&head), "{text}");
    assert!(text.contains("Status: passed"), "{text}");
    assert!(text.contains("a.txt"), "{text}");
    assert!(text.contains("No findings are recorded"), "{text}");

    let (ok, json, _) = show(&sandbox, "run-1", &["--json"]);
    assert!(ok);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["head_sha"], head.as_str());
    assert_eq!(value["status"], "current");
    assert_eq!(value["risks"], serde_json::json!([]));
    assert!(sandbox.data().join(format!("runs/run-1/review/{head}/review.md")).is_file());
}

#[test]
fn a_range_with_no_changes_reports_zero_files() {
    let sandbox = Sandbox::new("empty");
    let fixture = GitFixture::new();
    let head = git(fixture.root(), &["rev-parse", "HEAD"]);
    sandbox.write_run_file("run-2", "tests.json", &passing(&head));
    assert!(build(&sandbox, "run-2", fixture.root(), &head, &[]).status.success());
    let (_, json, _) = show(&sandbox, "run-2", &["--json"]);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["summary"]["files_changed"], 0);
}

#[test]
fn binary_and_renamed_files_are_reported_as_such() {
    let sandbox = Sandbox::new("binren");
    let fixture = GitFixture::new();
    std::fs::write(fixture.root().join("old name.txt"), "line1\nline2\nline3\nline4\n").unwrap();
    let base = commit(fixture.root(), "seed");
    git(fixture.root(), &["mv", "old name.txt", "new name.txt"]);
    std::fs::write(fixture.root().join("blob.bin"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();
    let head = commit(fixture.root(), "rename and binary");
    sandbox.write_run_file("run-3", "tests.json", &passing(&head));
    assert!(build(&sandbox, "run-3", fixture.root(), &base, &[]).status.success());
    let (_, text, _) = show(&sandbox, "run-3", &[]);
    assert!(text.contains("new name.txt (renamed from old name.txt)"), "{text}");
    assert!(text.contains("blob.bin binary"), "{text}");
}

#[test]
fn missing_tests_and_malformed_review_output_make_an_incomplete_report() {
    let sandbox = Sandbox::new("gaps");
    let fixture = GitFixture::new();
    let base = git(fixture.root(), &["rev-parse", "HEAD"]);
    std::fs::write(fixture.root().join("a.txt"), "x\n").unwrap();
    commit(fixture.root(), "change");
    let review = sandbox.write_run_file("run-4", "review-input.json", "this is not json");
    let built = build(&sandbox, "run-4", fixture.root(), &base, &["--review-input", review.to_str().unwrap()]);
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let (_, text, _) = show(&sandbox, "run-4", &[]);
    assert!(text.contains("State: incomplete"), "{text}");
    assert!(text.contains("Status: unknown"), "{text}");
    assert!(text.contains("Status: unavailable"), "{text}");
    assert!(text.contains("malformed"), "{text}");
}

#[test]
fn an_unknown_run_is_a_clear_error() {
    let sandbox = Sandbox::new("unknown");
    let (ok, _, err) = show(&sandbox, "no-such-run", &[]);
    assert!(!ok);
    assert!(err.contains("no review report") && err.contains("no-such-run"), "{err}");
}

#[test]
fn building_needs_no_provider_credentials_or_network() {
    let sandbox = Sandbox::new("offline");
    let fixture = GitFixture::new();
    let head = git(fixture.root(), &["rev-parse", "HEAD"]);
    let built = build(&sandbox, "run-5", fixture.root(), &head, &[]);
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
}
