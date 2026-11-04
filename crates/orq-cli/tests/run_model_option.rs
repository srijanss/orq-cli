use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const PR_URL: &str = "https://github.example/o/r/pull/7";

struct World {
    repo: GitFixture,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    base: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

fn world(tag: &str) -> World {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let base = orq_test_support::process_temp_dir().join(format!("orq-model-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("config/orq")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    let ssh = base.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    World { repo, state: base.join("state"), config: base.join("config"), ssh, base }
}

fn settings(w: &World, text: &str) {
    std::fs::write(w.repo.root().join(".orq/run.toml"), format!("gate = [\"true\"]\n{text}")).unwrap();
}

fn github() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build()
}

/// A `claude` that files each session under `with` (it was given the model) or `without`, then behaves as `agent`.
fn run(w: &World, extra: &[&str], model: &str) -> (Output, PathBuf, PathBuf) {
    run_with_fix(w, extra, model, "exit 0")
}

/// As `run`, but a session asked to repair a red gate runs `fix` in the worktree instead of the agent.
fn run_with_fix(w: &World, extra: &[&str], model: &str, fix: &str) -> (Output, PathBuf, PathBuf) {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let (with, without) = (w.base.join("with"), w.base.join("without"));
    let bin = w.base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\ncase \"$*\" in *'--model {model}'*) echo x >> '{}';; *) echo x >> '{}';; esac\ncase \"$*\" in *'local gate failed'*) {fix};; esac\nexec '{}' \"$@\"\n",
            with.display(),
            without.display(),
            agent.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let out = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .args(extra)
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("ORQ_MODEL")
        .output()
        .unwrap();
    (out, with, without)
}

fn sessions(file: &Path) -> usize {
    std::fs::read_to_string(file).map(|text| text.lines().count()).unwrap_or(0)
}

#[test]
fn the_model_flag_is_used_for_the_implement_session_and_the_review_draft() {
    let w = world("flag");
    settings(&w, "");

    let (out, with, without) = run(&w, &["--model", "custom-x"], "custom-x");

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "{}\n{stdout}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(sessions(&with), 2, "the implement session and the draft were given the model");
    assert_eq!(sessions(&without), 0, "no session ran on another model");
}

#[test]
fn the_model_setting_applies_and_the_flag_wins_over_it() {
    let w = world("setting");
    settings(&w, "model = \"from-file\"\n");
    let (out, with, without) = run(&w, &[], "from-file");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!((sessions(&with), sessions(&without)), (2, 0), "the setting was used");

    let w = world("flag-wins");
    settings(&w, "model = \"from-file\"\n");
    let (out, with, without) = run(&w, &["--model", "from-flag"], "from-flag");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!((sessions(&with), sessions(&without)), (2, 0), "the flag was used");
}

#[test]
fn a_model_that_could_be_an_option_is_refused_before_anything_starts() {
    let w = world("bad");
    settings(&w, "");

    let (out, with, without) = run(&w, &["--model", "--dangerous"], "--dangerous");

    assert_ne!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--model"), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!((sessions(&with), sessions(&without)), (0, 0), "no session was started");
}

#[test]
fn the_repair_session_for_a_red_gate_runs_on_the_run_model_and_the_flag_wins() {
    let w = world("fix");
    std::fs::write(w.repo.root().join(".orq/run.toml"), "gate = [\"test\", \"-f\", \"fixed\"]\nmodel = \"from-file\"\n").unwrap();
    let repair = "touch fixed; git add fixed; git -c user.name=t -c user.email=t@example.com commit -q -m 'fix: repair the gate'; exit 0";

    let (out, with, without) = run_with_fix(&w, &["--model", "from-flag"], "from-flag", repair);

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "{}\n{stdout}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(sessions(&with), 3, "the implement session, the repair session and the draft");
    assert_eq!(sessions(&without), 0, "no session ran on another model");
}

#[test]
fn the_repair_sessions_use_the_model_setting_and_a_gate_they_cannot_fix_fails_the_run() {
    let w = world("fix-red");
    std::fs::write(w.repo.root().join(".orq/run.toml"), "gate = [\"false\"]\nmodel = \"from-file\"\n").unwrap();

    let (out, with, without) = run_with_fix(&w, &[], "from-file", "exit 0");

    assert_eq!(out.status.code(), Some(31), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(sessions(&with) >= 2, "the implement session and at least one repair session were given the model");
    assert_eq!(sessions(&without), 0, "no session ran on another model");
}
