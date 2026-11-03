use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::GitFixture;

struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let home = orq_test_support::process_temp_dir().join(format!("orq-build-risks-{}-{tag}", std::process::id()));
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
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git").arg("-C").arg(repo).args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"]).args(args).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Build a report for a change that touches an auth file, with the given review input text.
fn built(tag: &str, review_input: &str) -> (Sandbox, serde_json::Value, String) {
    let sandbox = Sandbox::new(tag);
    let fixture = GitFixture::new();
    let base = git(fixture.root(), &["rev-parse", "HEAD"]);
    std::fs::create_dir_all(fixture.root().join("src/auth")).unwrap();
    std::fs::write(fixture.root().join("src/auth/login.rs"), "fn x() {}\n").unwrap();
    git(fixture.root(), &["add", "-A"]);
    git(fixture.root(), &["commit", "--quiet", "-m", "auth"]);
    let input = sandbox.home.join("review-input.json");
    std::fs::write(&input, review_input).unwrap();
    let out = sandbox.orq(&["review", "build", "run-1", "--repo", fixture.root().to_str().unwrap(), "--feature", "feat", "--base", &base, "--review-input", input.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let json = sandbox.orq(&["review", "show", "run-1", "--json"]);
    let text = sandbox.orq(&["review", "show", "run-1"]);
    let value = serde_json::from_slice(&json.stdout).unwrap();
    (sandbox, value, String::from_utf8_lossy(&text.stdout).into_owned())
}

#[test]
fn imported_findings_become_risks_and_the_changed_files_are_ranked() {
    let (_s, value, text) = built(
        "ok",
        r#"{"schema_version":1,"rounds":[{"findings":[{"id":"a","severity":"high","file":"src/auth/login.rs","line":1,"category":"security","claim":"token logged","status":"open","verdict":"CONFIRMED"}]}]}"#,
    );
    assert_eq!(value["risks"][0]["id"], "a");
    assert_eq!(value["risks"][0]["standing"], "confirmed");
    assert_eq!(value["file_risks"]["files"][0]["path"], "src/auth/login.rs");
    assert_eq!(value["file_risks"]["files"][0]["level"], "high");
    assert!(text.contains("token logged") && text.contains("Files needing attention"), "{text}");
    assert!(!text.contains("No findings are recorded"), "{text}");
}

#[test]
fn unreadable_findings_are_a_named_gap_not_a_clean_report() {
    let (_s, value, text) = built("bad", r#"{"schema_version":1,"rounds":[{"findings":[{"claim":"no id"}]}]}"#);
    assert_eq!(value["status"], "incomplete");
    assert!(value["gaps"].as_array().unwrap().iter().any(|g| g.as_str().unwrap().contains("findings")), "{value}");
    assert!(text.contains("findings"), "{text}");
}
