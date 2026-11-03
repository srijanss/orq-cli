use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::GitFixture;

struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let home = orq_test_support::process_temp_dir().join(format!("orq-review-fail-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        Sandbox { home }
    }

    fn report_dir(&self, run: &str, head: &str) -> PathBuf {
        self.home.join("data/orq/runs").join(run).join("review").join(head)
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
        command.output().unwrap()
    }

    fn build(&self, run: &str, repo: &Path, head: &str) -> Output {
        self.orq(&["review", "build", run, "--repo", repo.to_str().unwrap(), "--feature", "feat", "--base", head])
    }
}

fn head_of(repo: &Path) -> String {
    let output = Command::new("git").arg("-C").arg(repo).args(["rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_failed_markdown_write_publishes_nothing_and_a_retry_succeeds() {
    let sandbox = Sandbox::new("md");
    let fixture = GitFixture::new();
    let head = head_of(fixture.root());
    let blocker = sandbox.report_dir("run-6", &head).join("review.md");
    std::fs::create_dir_all(blocker.join("occupied")).unwrap();

    let failed = sandbox.build("run-6", fixture.root(), &head);
    assert!(!failed.status.success());
    assert!(stderr(&failed).contains("readable report"), "{}", stderr(&failed));
    let shown = sandbox.orq(&["review", "show", "run-6"]);
    assert!(!shown.status.success());
    assert!(stderr(&shown).contains("no review report"), "{}", stderr(&shown));
    assert!(!sandbox.report_dir("run-6", &head).join("review.json").exists());

    std::fs::remove_dir_all(&blocker).unwrap();
    let retried = sandbox.build("run-6", fixture.root(), &head);
    assert!(retried.status.success(), "{}", stderr(&retried));
    assert!(sandbox.orq(&["review", "show", "run-6"]).status.success());
}

#[test]
fn a_corrupt_stored_report_is_a_clear_error_in_both_formats() {
    let sandbox = Sandbox::new("corrupt");
    let fixture = GitFixture::new();
    let head = head_of(fixture.root());
    assert!(sandbox.build("run-7", fixture.root(), &head).status.success());
    std::fs::write(sandbox.report_dir("run-7", &head).join("review.json"), "{ not json").unwrap();

    for extra in [&[][..], &["--json"][..]] {
        let mut args = vec!["review", "show", "run-7"];
        args.extend_from_slice(extra);
        let shown = sandbox.orq(&args);
        assert!(!shown.status.success());
        assert!(stderr(&shown).contains("unreadable") && stderr(&shown).contains("run-7"), "{}", stderr(&shown));
        assert!(shown.stdout.is_empty());
    }
}
