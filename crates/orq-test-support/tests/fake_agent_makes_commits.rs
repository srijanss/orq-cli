use std::path::Path;
use std::process::Command;

use orq_test_support::{FakeAgent, GitFixture};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn a_scripted_agent_creates_a_file_and_commits_it_in_its_cwd() {
    let fixture = GitFixture::new();
    let agent = FakeAgent::builder()
        .write_file("src/new.rs", "fn main() {}\n")
        .commit("add the new file")
        .build();

    let output = Command::new(agent.path()).current_dir(fixture.root()).output().unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(git(fixture.root(), &["rev-list", "--count", "HEAD"]), "2");
    assert_eq!(git(fixture.root(), &["log", "-1", "--format=%s"]), "add the new file");
    assert_eq!(git(fixture.root(), &["show", "HEAD:src/new.rs"]), "fn main() {}");
    assert_eq!(git(fixture.root(), &["status", "--porcelain"]), "");
}

#[test]
fn steps_run_in_the_order_they_were_scripted() {
    let fixture = GitFixture::new();
    let agent = FakeAgent::builder()
        .write_file("a.txt", "one\n")
        .commit("first")
        .write_file("b.txt", "two\n")
        .commit("second")
        .build();

    Command::new(agent.path()).current_dir(fixture.root()).output().unwrap();

    assert_eq!(git(fixture.root(), &["log", "--format=%s", "-2"]), "second\nfirst");
    assert_eq!(git(fixture.root(), &["show", "--name-only", "--format=", "HEAD"]), "b.txt");
    assert_eq!(git(fixture.root(), &["show", "--name-only", "--format=", "HEAD~1"]), "a.txt");
}

#[test]
fn a_file_written_after_the_last_commit_stays_uncommitted() {
    let fixture = GitFixture::new();
    let agent = FakeAgent::builder()
        .write_file("done.txt", "committed\n")
        .commit("work")
        .write_file("leftover.txt", "not committed\n")
        .build();

    Command::new(agent.path()).current_dir(fixture.root()).output().unwrap();

    assert_eq!(git(fixture.root(), &["rev-list", "--count", "HEAD"]), "2");
    assert_eq!(git(fixture.root(), &["status", "--porcelain"]), "?? leftover.txt");
    assert_eq!(
        std::fs::read_to_string(fixture.root().join("leftover.txt")).unwrap(),
        "not committed\n"
    );
}

#[test]
fn an_agent_that_only_writes_leaves_no_new_commit() {
    let fixture = GitFixture::new();
    let agent = FakeAgent::builder().write_file("x.txt", "x\n").build();

    Command::new(agent.path()).current_dir(fixture.root()).output().unwrap();

    assert_eq!(git(fixture.root(), &["rev-list", "--count", "HEAD"]), "1");
}

#[test]
fn file_contents_and_messages_are_taken_literally() {
    let fixture = GitFixture::new();
    let body = "it's $HOME `date` \"quoted\" \\ backslash\n\nsecond paragraph\n";
    let message = "fix: it's \"quoted\" $(whoami)\n\nbody line with `ticks`";
    let agent = FakeAgent::builder()
        .write_file("odd name 'x'.txt", body)
        .commit(message)
        .build();

    Command::new(agent.path()).current_dir(fixture.root()).output().unwrap();

    assert_eq!(
        std::fs::read_to_string(fixture.root().join("odd name 'x'.txt")).unwrap(),
        body
    );
    assert_eq!(git(fixture.root(), &["log", "-1", "--format=%B"]), message);
}

#[test]
fn the_session_id_is_printed_as_a_json_line_before_the_other_output() {
    let agent = FakeAgent::builder().session_id("sess-42_a").stdout("done\n").build();

    let output = Command::new(agent.path()).output().unwrap();

    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "{\"session_id\":\"sess-42_a\"}\ndone\n"
    );
}

#[test]
fn a_commit_step_outside_a_repository_fails_loudly() {
    let dir = std::env::temp_dir().join(format!("orq-no-repo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let agent = FakeAgent::builder().write_file("x.txt", "x\n").commit("nope").build();

    let output = Command::new(agent.path())
        .current_dir(&dir)
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(70));
    assert!(String::from_utf8_lossy(&output.stderr).contains("fake agent: commit failed"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unsafe_step_arguments_are_rejected_when_the_agent_is_built() {
    for bad in ["", "/etc/passwd", "../escape", "a/../../b", "a/./b", "a\nb"] {
        let result = std::panic::catch_unwind(|| FakeAgent::builder().write_file(bad, "x").build());
        assert!(result.is_err(), "{bad:?} should be rejected");
    }
    for bad in ["", "has space", "quo\"te", "new\nline"] {
        let result = std::panic::catch_unwind(|| FakeAgent::builder().session_id(bad).build());
        assert!(result.is_err(), "session id {bad:?} should be rejected");
    }
}

#[test]
fn a_write_that_fails_stops_the_agent_with_a_diagnostic_and_skips_later_steps() {
    let fixture = GitFixture::new();
    // README.md is a regular file, so nothing can be created beneath it.
    let agent = FakeAgent::builder()
        .write_file("README.md/inside.txt", "x\n")
        .write_file("after.txt", "never written\n")
        .commit("never made")
        .stdout("should not be printed\n")
        .build();

    let output = Command::new(agent.path()).current_dir(fixture.root()).output().unwrap();

    assert_eq!(output.status.code(), Some(70));
    assert!(String::from_utf8_lossy(&output.stderr).contains("fake agent: write failed: README.md/inside.txt"));
    assert!(output.stdout.is_empty());
    assert!(!fixture.root().join("after.txt").exists());
    assert_eq!(git(fixture.root(), &["rev-list", "--count", "HEAD"]), "1");
}
