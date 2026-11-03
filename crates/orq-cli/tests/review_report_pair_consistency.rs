use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::GitFixture;

struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let home = orq_test_support::process_temp_dir().join(format!("orq-review-pair-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        Sandbox { home }
    }

    fn report_dir(&self, run: &str, head: &str) -> PathBuf {
        self.home.join("data/orq/runs").join(run).join("review").join(head)
    }

    fn build(&self, run: &str, repo: &Path, head: &str, feature: &str) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
        command.args(["review", "build", run, "--repo", repo.to_str().unwrap(), "--feature", feature, "--base", head]);
        command.env("HOME", &self.home);
        command.env("XDG_DATA_HOME", self.home.join("data"));
        command.env("XDG_STATE_HOME", self.home.join("state"));
        command.env("XDG_CONFIG_HOME", self.home.join("config"));
        command.env("XDG_CACHE_HOME", self.home.join("cache"));
        command.env_remove("HERDR_ACTIVE_PANE_CWD");
        command.output().unwrap()
    }
}

fn head_of(repo: &Path) -> String {
    let output = Command::new("git").arg("-C").arg(repo).args(["rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn a_failed_rebuild_leaves_the_published_pair_unchanged() {
    let sandbox = Sandbox::new("rebuild");
    let fixture = GitFixture::new();
    let head = head_of(fixture.root());
    assert!(sandbox.build("run-8", fixture.root(), &head, "first").status.success());
    let dir = sandbox.report_dir("run-8", &head);
    let markdown_before = std::fs::read_to_string(dir.join("review.md")).unwrap();

    // Make publishing the JSON fail after the Markdown has been written.
    std::fs::remove_file(dir.join("review.json")).unwrap();
    std::fs::create_dir_all(dir.join("review.json").join("occupied")).unwrap();

    let failed = sandbox.build("run-8", fixture.root(), &head, "second");
    assert!(!failed.status.success());
    assert_eq!(std::fs::read_to_string(dir.join("review.md")).unwrap(), markdown_before);
}

#[test]
fn a_failed_first_build_leaves_no_markdown_behind() {
    let sandbox = Sandbox::new("first");
    let fixture = GitFixture::new();
    let head = head_of(fixture.root());
    let dir = sandbox.report_dir("run-9", &head);
    std::fs::create_dir_all(dir.join("review.json").join("occupied")).unwrap();

    let failed = sandbox.build("run-9", fixture.root(), &head, "first");
    assert!(!failed.status.success());
    assert!(!dir.join("review.md").exists());
}
