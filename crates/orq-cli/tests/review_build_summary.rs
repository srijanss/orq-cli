use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::GitFixture;

struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let home = orq_test_support::process_temp_dir().join(format!("orq-summary-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::create_dir_all(home.join("config/orq")).unwrap();
        Sandbox { home }
    }

    fn config(&self, text: &str) {
        std::fs::write(self.home.join("config/orq/config.toml"), text).unwrap();
    }

    /// A fake `claude` that records its arguments and prints `answer`.
    fn fake_claude(&self, body: &str) {
        let path = self.home.join("bin/claude");
        std::fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\n{body}\n", self.home.join("argv.txt").display())).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn orq(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
        command.args(args);
        command.env("HOME", &self.home);
        command.env("XDG_DATA_HOME", self.home.join("data"));
        command.env("XDG_STATE_HOME", self.home.join("state"));
        command.env("XDG_CONFIG_HOME", self.home.join("config"));
        command.env("XDG_CACHE_HOME", self.home.join("cache"));
        command.env("PATH", format!("{}:{}", self.home.join("bin").display(), std::env::var("PATH").unwrap()));
        command.env_remove("HERDR_ACTIVE_PANE_CWD");
        command.output().unwrap()
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git").arg("-C").arg(repo).args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"]).args(args).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn report(sandbox: &Sandbox) -> serde_json::Value {
    let fixture = GitFixture::new();
    let base = git(fixture.root(), &["rev-parse", "HEAD"]);
    std::fs::write(fixture.root().join("a.rs"), "fn a() {}\n").unwrap();
    git(fixture.root(), &["add", "-A"]);
    git(fixture.root(), &["commit", "--quiet", "-m", "a"]);
    let args = ["review", "build", "run-1", "--repo", fixture.root().to_str().unwrap(), "--feature", "feat", "--base", &base];
    let out = sandbox.orq(&args);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let shown = sandbox.orq(&["review", "show", "run-1", "--json"]);
    serde_json::from_slice(&shown.stdout).unwrap()
}

#[test]
fn no_model_is_run_unless_the_config_enables_the_summary() {
    let sandbox = Sandbox::new("off");
    sandbox.fake_claude("echo should not run");
    let value = report(&sandbox);
    assert!(value.get("generated_summary").is_none(), "{value}");
    assert!(!sandbox.home.join("argv.txt").exists(), "claude was started");
}

#[test]
fn an_enabled_summary_is_generated_labelled_and_run_without_tools() {
    let sandbox = Sandbox::new("on");
    sandbox.config("[review_summary]\nenabled = true\nmodel = \"haiku\"\n");
    sandbox.fake_claude("echo 'One file changed.'");
    let value = report(&sandbox);
    assert_eq!(value["generated_summary"]["generated"], true, "{value}");
    assert_eq!(value["generated_summary"]["model"], "haiku");
    assert_eq!(value["generated_summary"]["text"], "One file changed.");
    let argv = std::fs::read_to_string(sandbox.home.join("argv.txt")).unwrap();
    assert!(argv.contains("--setting-sources") && argv.contains("a.rs"), "{argv}");
}

#[test]
fn a_failing_model_or_bad_config_still_builds_the_report_without_a_summary() {
    let sandbox = Sandbox::new("fail");
    sandbox.config("[review_summary]\nenabled = true\nmodel = \"haiku\"\n");
    sandbox.fake_claude("exit 3");
    let value = report(&sandbox);
    assert!(value.get("generated_summary").is_none(), "{value}");

    let sandbox = Sandbox::new("bad");
    sandbox.config("[review_summary]\nenabled = true\n");
    sandbox.fake_claude("echo nope");
    let value = report(&sandbox);
    assert!(value.get("generated_summary").is_none(), "{value}");
    assert!(value["gaps"].to_string().contains("review_summary"), "{value}");
}
