//! A feature the implement session marked `completed` is not done until its run delivers: while the
//! gate (verify), its fix sessions and the review run, the ledger shows it `awaiting_delivery`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const PR_URL: &str = "https://github.example/o/r/pull/7";

struct World {
    repo: GitFixture,
    _remote: PathBuf,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    bin: PathBuf,
    /// Where the gate copies the ledger as it finds it.
    seen: PathBuf,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The status the ledger at `path` records for `name`.
fn status_in(path: &Path, name: &str) -> String {
    let ledger: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let entry = ledger
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["featureName"] == name)
        .unwrap();
    entry["status"].as_str().unwrap().to_string()
}

fn status_of(repo: &Path, name: &str) -> String {
    status_in(&repo.join(".tdd-features.json"), name)
}

/// How the gate and the gate's fix sessions behave.
struct Gate {
    /// The gate's shell body; `$LEDGER` and `$SEEN` name the main ledger and the file it copies to.
    body: &'static str,
    /// What a fix session after a red gate does (shell) before it exits 0.
    fix: &'static str,
}

/// A gate that copies the ledger and exits `exit`; its fix sessions change nothing.
fn plain_gate(exit: i32) -> Gate {
    Gate {
        body: if exit == 0 {
            "cp \"$LEDGER\" \"$SEEN\""
        } else {
            "cp \"$LEDGER\" \"$SEEN\"; exit 1"
        },
        fix: ":",
    }
}

/// A repository with a pushed `main` whose ledger has `entries`, a `claude` that marks every
/// `pending` feature `completed` in the main ledger, as the TDD workflow does when the session
/// finishes, before it runs `agent`, and the `gate`.
fn world(tag: &str, agent: &FakeAgent, entries: &[LedgerEntry], gate: Gate) -> World {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-run-verify-held-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config/orq")).unwrap();
    std::fs::create_dir_all(root.join("state")).unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(
        repo.root(),
        &[
            "remote",
            "set-url",
            "origin",
            &format!("forge.example:{}", remote.display()),
        ],
    );
    let ssh = root.join("ssh");
    std::fs::write(
        &ssh,
        "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), entries);
    let ledger = repo.root().join(".tdd-features.json");
    let claude = root.join("bin/claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nLEDGER='{ledger}'\ncase \"$*\" in *'local gate failed'*) {fix}; exit 0;; esac\nsed -i 's/\"pending\"/\"completed\"/' \"$LEDGER\"\nexec '{agent}' \"$@\"\n",
            ledger = ledger.display(),
            fix = gate.fix,
            agent = agent.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let seen = root.join("seen.json");
    let body = format!(
        "LEDGER='{}'; SEEN='{}'; COUNT='{}'; {}",
        ledger.display(),
        seen.display(),
        root.join("gate-runs").display(),
        gate.body
    );
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    std::fs::write(
        repo.root().join(".orq/run.toml"),
        format!(
            "gate = [\"sh\", \"-c\", {}]\nexplain_failures = false\n",
            serde_json::to_string(&body).unwrap()
        ),
    )
    .unwrap();
    World {
        repo,
        _remote: remote,
        state: root.join("state"),
        config: root.join("config"),
        ssh,
        bin: root.join("bin"),
        seen,
    }
}

fn single(tag: &str, agent: &FakeAgent, gate: Gate) -> World {
    world(
        tag,
        agent,
        &[
            LedgerEntry::new("COM-01", "pending"),
            LedgerEntry::new("COM-02", "in_progress"),
        ],
        gate,
    )
}

fn committing_agent() -> FakeAgent {
    FakeAgent::builder()
        .write_file("feature.txt", "done\n")
        .commit("feat: add the feature")
        .build()
}

fn github() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(
            &["pr", "view"],
            &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"),
        )
        .build()
}

fn run(w: &World, provider: &FakeProvider, which: &[&str]) -> Output {
    let mut dirs = vec![w.bin.clone(), provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    orq_test_support::sandboxed_command(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(which)
        .args(["--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("TDD_FEATURES_PATH")
        .output()
        .unwrap()
}

#[test]
fn a_feature_the_session_completed_awaits_delivery_while_verify_runs() {
    let agent = committing_agent();
    let w = single("verify", &agent, plain_gate(0));

    let output = run(&w, &github(), &["--feature", "COM-01"]);

    assert!(
        w.seen.exists(),
        "the gate ran: {}\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    assert_eq!(
        status_in(&w.seen, "COM-01"),
        "awaiting_delivery",
        "{}",
        text(&output.stderr)
    );
}

#[test]
fn a_delivered_run_completes_the_feature_after_verify() {
    let agent = committing_agent();
    let w = single("delivered", &agent, plain_gate(0));

    let output = run(&w, &github(), &["--feature", "COM-01"]);

    assert!(
        text(&output.stdout).contains("state: review_ready"),
        "{}\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    assert_eq!(status_of(w.repo.root(), "COM-01"), "completed");
    assert_eq!(
        status_of(w.repo.root(), "COM-02"),
        "in_progress",
        "a feature outside the run is left alone"
    );
}

#[test]
fn a_run_whose_verify_fails_leaves_the_feature_awaiting_delivery() {
    let agent = committing_agent();
    let w = single("verify-fails", &agent, plain_gate(1));

    let output = run(&w, &github(), &["--feature", "COM-01"]);

    assert!(
        text(&output.stdout).contains("failed"),
        "{}\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    assert_eq!(
        status_of(w.repo.root(), "COM-01"),
        "awaiting_delivery",
        "{}",
        text(&output.stderr)
    );
    assert_eq!(
        status_of(w.repo.root(), "COM-02"),
        "in_progress",
        "a feature outside the run is left alone"
    );
}

#[test]
fn a_group_feature_the_session_completed_still_counts_for_the_group() {
    let agent = committing_agent();
    let w = world(
        "group",
        &agent,
        &[
            LedgerEntry::new("COM-01", "pending"),
            LedgerEntry::new("COM-02", "pending"),
        ],
        plain_gate(0),
    );

    let output = run(&w, &github(), &["--group", "COM-01,COM-02"]);

    let said = format!("{}\n{}", text(&output.stdout), text(&output.stderr));
    assert!(!said.contains("ended before"), "{said}");
    assert!(
        text(&output.stdout).contains("state: review_ready"),
        "{said}"
    );
    assert_eq!(
        status_in(&w.seen, "COM-01"),
        "awaiting_delivery",
        "held while verify ran"
    );
    assert_eq!(
        status_in(&w.seen, "COM-02"),
        "awaiting_delivery",
        "held while verify ran"
    );
    assert_eq!(status_of(w.repo.root(), "COM-01"), "completed");
    assert_eq!(status_of(w.repo.root(), "COM-02"), "completed");
}

/// A gate fix session that marks the run's feature `completed` again does not finish it either:
/// the gate that runs after it sees it `awaiting_delivery` again.
#[test]
fn a_feature_a_gate_fix_session_completed_awaits_delivery_when_the_gate_runs_again() {
    let agent = committing_agent();
    let gate = Gate {
        body:
            "if [ -e \"$COUNT\" ]; then cp \"$LEDGER\" \"$SEEN\"; else touch \"$COUNT\"; exit 1; fi",
        fix: "sed -i -E 's/\"(in_progress|awaiting_delivery)\"/\"completed\"/' \"$LEDGER\"",
    };
    let w = world(
        "gate-fix",
        &agent,
        &[LedgerEntry::new("COM-01", "pending")],
        gate,
    );

    let output = run(&w, &github(), &["--feature", "COM-01"]);

    assert!(
        w.seen.exists(),
        "the gate ran again: {}\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    assert_eq!(
        status_in(&w.seen, "COM-01"),
        "awaiting_delivery",
        "{}",
        text(&output.stderr)
    );
}
