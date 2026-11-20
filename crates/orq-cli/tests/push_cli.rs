//! `orq push --repo <path> [--branch <target>] [--yes]` pushes the project's target to origin after
//! the person sees the commits and answers `yes` (or passed `--yes`); a refusal prints its reason and
//! pushes nothing. The checkout's github.com origin is rewritten by git to a bare repository, and a
//! fake `gh` on PATH answers the branch lookup.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use orq_test_support::FakeProvider;

const ORIGIN: &str = "https://github.com/acme/alpha.git";
const LOOKUP: [&str; 2] = ["api", "repos/acme/alpha/branches/main"];

struct Sandbox {
    home: PathBuf,
    repo: PathBuf,
    origin: PathBuf,
    base: String,
    head: String,
    gh: FakeProvider,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, name: &str, subject: &str) -> String {
    std::fs::write(dir.join(name), format!("{subject}\n")).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-q", "-m", subject]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A checkout on `main` with two commits origin does not have; `protected` is what GitHub says.
fn sandbox(tag: &str, protected: bool) -> Sandbox {
    let home = orq_test_support::process_temp_dir().join(format!("orq-push-cli-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let origin = home.join("origin.git");
    let repo = home.join("alpha");
    std::fs::create_dir_all(&repo).unwrap();
    git(&home, &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["remote", "add", "origin", ORIGIN]);
    git(&repo, &["config", &format!("url.file://{}.insteadOf", origin.display()), ORIGIN]);
    let base = commit(&repo, "README", "start");
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);
    commit(&repo, "a.txt", "first change");
    let head = commit(&repo, "b.txt", "second change");
    let gh = FakeProvider::builder().reply(&LOOKUP, &format!(r#"{{"name":"main","protected":{protected}}}"#)).build();
    Sandbox { home, repo, origin, base, head, gh }
}

impl Sandbox {
    fn push(&self, args: &[&str], answer: &str) -> Output {
        let path = format!("{}:{}", self.gh.bin_dir().display(), std::env::var("PATH").unwrap_or_default());
        let mut child = Command::new(env!("CARGO_BIN_EXE_orq"))
            .arg("push")
            .arg("--repo")
            .arg(&self.repo)
            .args(args)
            .env("PATH", path)
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("XDG_STATE_HOME", self.home.join("state"))
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .env_remove("HERDR_ACTIVE_PANE_CWD")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // A refusal can end the command before it reads its answer.
        let _ = child.stdin.take().unwrap().write_all(answer.as_bytes());
        child.wait_with_output().unwrap()
    }

    fn origin_main(&self) -> String {
        git(&self.origin, &["rev-parse", "refs/heads/main"])
    }

    fn audit(&self) -> Vec<serde_json::Value> {
        let log = std::fs::read_to_string(self.home.join("state/orq/remote-actions.jsonl")).unwrap_or_default();
        log.lines().map(|line| serde_json::from_str(line).unwrap()).filter(|line: &serde_json::Value| line["action"] == "project.push_target").collect()
    }
}

#[test]
fn the_commits_are_listed_and_only_a_yes_answer_pushes_them() {
    let s = sandbox("yes", false);

    let declined = s.push(&[], "no\n");
    let stdout = String::from_utf8_lossy(&declined.stdout);
    assert!(!declined.status.success(), "{stdout}");
    assert!(stdout.contains("origin/main..main"), "{stdout}");
    assert!(stdout.contains(&s.head[..7]) && stdout.contains("second change") && stdout.contains("first change"), "{stdout}");
    assert!(stdout.contains("not from orq"), "{stdout}");
    assert!(stdout.contains("Push 2 commits"), "{stdout}");
    assert_eq!(s.origin_main(), s.base, "a no pushes nothing");

    let accepted = s.push(&[], "yes\n");
    assert!(accepted.status.success(), "{}", String::from_utf8_lossy(&accepted.stderr));
    assert!(String::from_utf8_lossy(&accepted.stdout).contains("Pushed 2 commits to origin/main"), "{}", String::from_utf8_lossy(&accepted.stdout));
    assert_eq!(s.origin_main(), s.head, "origin has the local target");
    let lines = s.audit();
    assert_eq!(lines.len(), 1, "{lines:?}");
    let reason = lines[0]["reason"].as_str().unwrap();
    assert!(reason.contains("from cli") && reason.contains(&s.head) && reason.contains(&s.base), "{reason}");
}

#[test]
fn yes_flag_pushes_without_asking() {
    let s = sandbox("yes-flag", false);
    let out = s.push(&["--yes", "--branch", "main"], "");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(s.origin_main(), s.head);
}

#[test]
fn a_refusal_prints_its_reason_and_pushes_nothing() {
    let s = sandbox("protected", true);
    let out = s.push(&["--yes"], "");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("main is protected on GitHub: deliver by review instead"), "{stderr}");
    assert_eq!(s.origin_main(), s.base);
}
