use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_core::review_lifecycle::ReviewLifecycle;
use orq_core::store::Store;
use orq_test_support::GitFixture;

struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let home = orq_test_support::process_temp_dir().join(format!("orq-permalinks-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        Sandbox { home }
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

    fn data(&self) -> PathBuf {
        self.home.join("data/orq")
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git").arg("-C").arg(repo).args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"]).args(args).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

const INPUT: &str = r#"{"schema_version":1,"head_sha":"HEAD","rounds":[{"findings":[
{"id":"a","severity":"high","file":"src/auth/login.rs","line":1,"category":"security","claim":"x","status":"open","verdict":"CONFIRMED"},
{"id":"b","severity":"low","file":"README.md","line":1,"category":"bugs","claim":"y","status":"open"}]}]}"#;

/// Builds a current report; the stored provider review (if any) is saved before the second build.
fn build(tag: &str, provider_review: Option<(&str, &str)>) -> (serde_json::Value, String) {
    let sandbox = Sandbox::new(tag);
    let fixture = GitFixture::new();
    let base = git(fixture.root(), &["rev-parse", "HEAD"]);
    std::fs::create_dir_all(fixture.root().join("src/auth")).unwrap();
    std::fs::write(fixture.root().join("src/auth/login.rs"), "fn x() {}\n").unwrap();
    // The commit deletes README.md, so it is a changed file that is absent from the reviewed tree.
    git(fixture.root(), &["rm", "--quiet", "README.md"]);
    git(fixture.root(), &["add", "-A"]);
    git(fixture.root(), &["commit", "--quiet", "-m", "auth"]);
    let head = git(fixture.root(), &["rev-parse", "HEAD"]);
    // Present again in the working tree, but not in the reviewed commit.
    std::fs::write(fixture.root().join("README.md"), "back\n").unwrap();

    let run_dir = sandbox.data().join("runs/run-1");
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(run_dir.join("tests.json"), format!(r#"{{"schema_version":1,"head_sha":"{head}","commands":[{{"command":"cargo test","exit_code":0}}]}}"#)).unwrap();
    let input = sandbox.home.join("review-input.json");
    std::fs::write(&input, INPUT.replace("HEAD", &head)).unwrap();
    let args = ["review", "build", "run-1", "--repo", fixture.root().to_str().unwrap(), "--feature", "feat", "--base", &base, "--review-input", input.to_str().unwrap()];

    assert!(sandbox.orq(&args).status.success());
    if let Some((provider, url)) = provider_review {
        let store = Store::open(&sandbox.data().join("orq.db")).unwrap();
        store.save_lifecycle("run-1", &ReviewLifecycle::associate(provider, "12", url, &head)).unwrap();
    }
    let out = sandbox.orq(&args);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let shown = sandbox.orq(&["review", "show", "run-1", "--json"]);
    (serde_json::from_slice(&shown.stdout).unwrap(), head)
}

#[test]
fn a_file_in_the_reviewed_commit_links_and_one_only_in_the_working_tree_does_not() {
    let (value, head) = build("linked", Some(("github", "https://github.example/o/r/pull/12")));
    assert_eq!(value["status"], "current", "{value}");
    assert_eq!(value["file_risks"]["files"][0]["link"], format!("https://github.example/o/r/blob/{head}/src/auth/login.rs"));
    assert_eq!(value["risks"][0]["link"], format!("https://github.example/o/r/blob/{head}/src/auth/login.rs#L1"));
    assert!(value["risks"][1]["link"].is_null());
    assert!(value["risks"][1]["link_reason"].as_str().unwrap().contains("cannot be linked"));
}

#[test]
fn without_a_stored_provider_review_the_report_builds_with_no_links_and_a_reason() {
    let (value, _) = build("unlinked", None);
    assert!(value["file_risks"]["files"][0]["link"].is_null());
    assert!(value["file_risks"]["files"][0]["link_reason"].as_str().unwrap().contains("no provider review"));
}
