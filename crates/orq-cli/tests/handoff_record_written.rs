use orq_orchestrator::RunStore;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const PR_URL: &str = "https://github.example/o/r/pull/7";

#[allow(dead_code)]
struct World {
    repo: GitFixture,
    remote: PathBuf,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    base: PathBuf,
    provider: FakeProvider,
    path: std::ffi::OsString,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn world(tag: &str, agent: &FakeAgent) -> World {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let base = orq_test_support::process_temp_dir().join(format!("orq-reviewer-switch-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("config/orq")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    let ssh = base.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);

    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build();
    let bin = base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, format!("#!/bin/sh\ncase \"$*\" in *'local gate failed'*) exit 0;; esac\nexec '{}' \"$@\"\n", agent.path().display())).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(dirs).unwrap();
    World { repo, remote, state: base.join("state"), config: base.join("config"), ssh, base, provider, path }
}

fn orq(w: &World, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(args)
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &w.path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

/// A gate that counts its runs and asks the run it belongs to to pause at its next boundary.
fn pausing_gate(w: &World) -> PathBuf {
    let count = w.base.join("gate-runs");
    let script = w.base.join("gate.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho run >> '{count}'\nXDG_STATE_HOME='{state}' '{orq}' run pause \"$(ls '{state}/orq/runs')\" >'{pause_log}' 2>&1\nexit 0\n",
            count = count.display(),
            orq = env!("CARGO_BIN_EXE_orq"),
            state = w.state.display(),
            pause_log = w.base.join("pause.log").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), format!("gate = [\"{}\"]\nexplain_failures = false\n", script.display())).unwrap();
    count
}

fn agent() -> FakeAgent {
    FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build()
}



const PROFILES: &str = "[models.fast-claude]\nharness = \"claude\"\nmodel = \"sonnet\"\n\n[models.slow-claude]\nharness = \"claude\"\nmodel = \"opus\"\n\n[models.team-pi]\nharness = \"pi\"\nmodel = \"team/model\"\n";

fn resumed_run(store: &RunStore, first: &str) -> String {
    store.run_ids().unwrap().into_iter().find(|id| id != first).expect("a resumed run")
}

fn request(w: &World, run_id: &str, role: &str, profile: &str) {
    let out = orq(w, &["run", "switch", run_id, "--role", role, "--profile", profile, "--repo", w.repo.root().to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_switch_writes_a_handoff_with_the_plan_the_diff_the_gate_and_the_checkout_but_no_transcript() {
    let agent = agent();
    let w = world("handoff", &agent);
    std::fs::write(w.repo.root().join(".orq.toml"), PROFILES).unwrap();
    pausing_gate(&w);
    let first = orq(&w, &["run", "--repo", w.repo.root().to_str().unwrap(), "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"]);
    assert_eq!(first.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&first.stdout), String::from_utf8_lossy(&first.stderr));
    let stdout = String::from_utf8_lossy(&first.stdout).into_owned();
    let pause_log = std::fs::read_to_string(w.base.join("pause.log")).unwrap_or_default();
    assert!(stdout.contains("state: paused"), "{stdout}\npause: {pause_log}");
    let store = RunStore::new(&w.state.join("orq"));
    let run_id = store.run_ids().unwrap().remove(0);
    let paused = store.load(&run_id).unwrap();

    request(&w, &run_id, "implementer", "fast-claude");
    let out = orq(&w, &["run", "resume", &run_id, "--yes", "--no-ci-check", "--repo", w.repo.root().to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));

    let new_id = resumed_run(&store, &run_id);
    let path = w.state.join("orq").join("runs").join(&new_id).join("handoff.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| { let ev = std::fs::read_to_string(path.parent().unwrap().join("events.jsonl")).unwrap_or_default(); let kinds: Vec<String> = ev.lines().map(|l| { let v: serde_json::Value = serde_json::from_str(l).unwrap(); format!("{} {}", v["type"], v["payload"]) }).collect(); panic!("no handoff.json: {e}
{}
{}", kinds.join("
"), String::from_utf8_lossy(&out.stderr)) });
    let handoff: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(handoff["from_run"], run_id.as_str());
    assert_eq!(handoff["branch"], paused.branch.as_str());
    assert_eq!(handoff["worktree"], paused.worktree.to_str().unwrap());
    let plan = handoff["plan"].as_array().expect("the feature plan");
    assert!(plan.iter().any(|f| f["id"] == "COM-01" && f["status"] == "pending"), "{plan:?}");
    let files = handoff["diff"]["files"].as_array().expect("the diff against the base");
    assert!(files.iter().any(|f| f["path"] == "feature.txt"), "{files:?}");
    assert_eq!(handoff["gate"]["outcome"], "passed", "{}", handoff["gate"]);
    // A handoff carries state, not the conversation: no transcript or session ids.
    for key in ["transcript", "messages", "conversation", "session_id", "sessions"] {
        assert!(handoff.get(key).is_none(), "handoff must not hold {key}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
