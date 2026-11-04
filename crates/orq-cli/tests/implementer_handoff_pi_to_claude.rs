use orq_orchestrator::RunStore;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const PR_URL: &str = "https://github.example/o/r/pull/7";
const PROFILES: &str = "[models.fast-claude]\nharness = \"claude\"\nmodel = \"sonnet\"\n\n[models.team-pi]\nharness = \"pi\"\nmodel = \"team/model\"\n";

struct World {
    repo: GitFixture,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    base: PathBuf,
    path: std::ffi::OsString,
    _provider: FakeProvider,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stub(dir: &Path, name: &str, target: &Path) {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\ncase \"$*\" in *'local gate failed'*) exit 0;; esac\nexec '{}' \"$@\"\n", target.display())).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

fn world(tag: &str, claude: &FakeAgent, pi: &FakeAgent) -> World {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let base = orq_test_support::process_temp_dir().join(format!("orq-pi-to-claude-{}-{tag}", std::process::id()));
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
    stub(&bin, "claude", claude.path());
    stub(&bin, "pi", pi.path());
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(dirs).unwrap();
    World { repo, state: base.join("state"), config: base.join("config"), ssh, base, path, _provider: provider }
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

/// A gate that asks the run to pause at its next boundary, and only on its first run.
fn pausing_once_gate(w: &World) {
    let count = w.base.join("gate-runs");
    let script = w.base.join("gate.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho run >> '{count}'\n[ \"$(wc -l < '{count}')\" -eq 1 ] && XDG_STATE_HOME='{state}' '{orq}' run pause \"$(ls '{state}/orq/runs')\" >'{pause_log}' 2>&1\nexit 0\n",
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
}

#[test]
fn after_a_switch_from_pi_to_claude_the_claude_session_continues_in_the_same_worktree_from_the_handoff() {
    let claude = FakeAgent::builder().write_file("claude.txt", "from claude\n").commit("feat: claude continues the work").build();
    let pi = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: pi starts the feature").build();
    let w = world("pi-to-claude", &claude, &pi);
    std::fs::write(w.repo.root().join(".orq.toml"), PROFILES).unwrap();
    pausing_once_gate(&w);
    let repo = w.repo.root().to_str().unwrap().to_string();
    let first = orq(&w, &["run", "--repo", &repo, "--feature", "COM-01", "--implementer", "team-pi", "--remote", "origin", "--target", "main", "--no-ci-check"]);
    assert_eq!(first.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&first.stdout), String::from_utf8_lossy(&first.stderr));
    let store = RunStore::new(&w.state.join("orq"));
    let run_id = store.run_ids().unwrap().remove(0);
    let paused = store.load(&run_id).unwrap();
    assert!(pi.first_run().is_some(), "pi implemented the feature first");
    assert!(claude.last_run().is_none(), "claude must not run before the switch");

    let switch = orq(&w, &["run", "switch", &run_id, "--role", "implementer", "--profile", "fast-claude", "--repo", &repo]);
    assert!(switch.status.success(), "{}", String::from_utf8_lossy(&switch.stderr));
    let out = orq(&w, &["run", "resume", &run_id, "--yes", "--no-ci-check", "--repo", &repo]);
    assert_eq!(out.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));

    let run = claude.first_run().expect("a claude implement session ran after the switch");
    assert!(run.cwd.ends_with(paused.worktree.file_name().unwrap()), "same worktree: {} vs {}", run.cwd.display(), paused.worktree.display());
    let prompt = run.args.join(" ");
    assert!(prompt.contains("--model sonnet"), "{prompt}");
    assert!(prompt.contains("Handoff from the previous session:"), "{prompt}");
    assert!(prompt.contains("COM-01"), "{prompt}");
    let log = git(w.repo.root(), &["log", "--format=%s", &paused.branch]);
    assert!(log.contains("feat: claude continues the work"), "the claude commit lands on the run's branch: {log}");
    let resumed = store.run_ids().unwrap().into_iter().find(|id| *id != run_id).expect("a resumed run");
    assert_eq!(store.load(&resumed).unwrap().branch, paused.branch);
}
