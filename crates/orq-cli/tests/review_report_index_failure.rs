use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::GitFixture;

struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let home = orq_test_support::process_temp_dir().join(format!("orq-review-index-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        Sandbox { home }
    }

    fn data(&self) -> PathBuf {
        self.home.join("data/orq")
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

    fn build(&self, run: &str, repo: &Path, head: &str, feature: &str) -> Output {
        self.orq(&["review", "build", run, "--repo", repo.to_str().unwrap(), "--feature", feature, "--base", head])
    }
}

fn head_of(repo: &Path) -> String {
    let output = Command::new("git").arg("-C").arg(repo).args(["rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn reject_inserts(db: &Path) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch("CREATE TRIGGER no_inserts BEFORE INSERT ON reviews BEGIN SELECT RAISE(ABORT, 'index unavailable'); END;").unwrap();
}

#[test]
fn an_indexing_failure_on_rebuild_keeps_the_published_report_unchanged() {
    let sandbox = Sandbox::new("rebuild");
    let fixture = GitFixture::new();
    let head = head_of(fixture.root());
    assert!(sandbox.build("run-10", fixture.root(), &head, "first").status.success());
    let dir = sandbox.data().join("runs/run-10/review").join(&head);
    let json_before = std::fs::read(dir.join("review.json")).unwrap();
    let markdown_before = std::fs::read(dir.join("review.md")).unwrap();
    let shown_before = sandbox.orq(&["review", "show", "run-10"]);

    reject_inserts(&sandbox.data().join("orq.db"));
    let failed = sandbox.build("run-10", fixture.root(), &head, "second");

    assert!(!failed.status.success());
    assert_eq!(std::fs::read(dir.join("review.json")).unwrap(), json_before);
    assert_eq!(std::fs::read(dir.join("review.md")).unwrap(), markdown_before);
    assert_eq!(sandbox.orq(&["review", "show", "run-10"]).stdout, shown_before.stdout);
}

#[test]
fn an_indexing_failure_on_a_first_build_publishes_nothing() {
    let sandbox = Sandbox::new("first");
    let fixture = GitFixture::new();
    let head = head_of(fixture.root());
    // Create the database, then make it refuse the index insert.
    assert!(sandbox.orq(&["review", "show", "run-11"]).status.code().is_some());
    reject_inserts(&sandbox.data().join("orq.db"));

    let failed = sandbox.build("run-11", fixture.root(), &head, "first");

    assert!(!failed.status.success());
    let dir = sandbox.data().join("runs/run-11/review").join(&head);
    assert!(!dir.join("review.json").exists());
    assert!(!dir.join("review.md").exists());
}
