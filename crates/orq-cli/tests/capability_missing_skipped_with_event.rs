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
    let base = orq_test_support::process_temp_dir().join(format!("orq-capability-skipped-{}-{tag}", std::process::id()));
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
    World { repo, state: base.join("state"), config: base.join("config"), ssh, path, _provider: provider }
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


fn run_with(tag: &str, profile: &str, run_toml: Option<&str>) -> (World, Vec<orq_orchestrator::Event>) {
    let claude = FakeAgent::builder().write_file("claude.txt", "c\n").commit("feat: claude").build();
    let pi = FakeAgent::builder().write_file("pi.txt", "p\n").commit("feat: pi").build();
    let w = world(tag, &claude, &pi);
    std::fs::write(w.repo.root().join(".orq.toml"), PROFILES).unwrap();
    if let Some(body) = run_toml {
        std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
        std::fs::write(w.repo.root().join(".orq/run.toml"), body).unwrap();
    }
    let repo = w.repo.root().to_str().unwrap().to_string();
    let out = orq(&w, &["run", "--repo", &repo, "--feature", "COM-01", "--implementer", profile, "--remote", "origin", "--target", "main", "--no-ci-check"]);
    assert_eq!(out.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let store = RunStore::new(&w.state.join("orq"));
    let run_id = store.run_ids().unwrap().remove(0);
    let events = store.events(&run_id).unwrap();
    // keep agents alive until the run is read
    drop((claude, pi));
    (w, events)
}

fn skipped(events: &[orq_orchestrator::Event]) -> Vec<&orq_orchestrator::Event> {
    events.iter().filter(|e| e.kind == "capability.skipped").collect()
}

#[test]
fn a_harness_without_remote_control_skips_it_and_the_run_records_which_harness_and_capability() {
    let (_w, events) = run_with("pi", "team-pi", None);
    let skipped = skipped(&events);
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!(skipped[0].payload["harness"], "pi");
    assert_eq!(skipped[0].payload["capability"], "remote_control");
    assert_eq!(skipped[0].stage.as_deref(), Some("implement_and_review"));
}

#[test]
fn a_harness_that_has_the_capability_or_a_run_that_does_not_ask_for_it_records_no_skip() {
    let (_w, claude_events) = run_with("claude", "fast-claude", None);
    assert!(skipped(&claude_events).is_empty(), "{claude_events:?}");
    let (_w2, off_events) = run_with("pi-off", "team-pi", Some("remote_control = false\n"));
    assert!(skipped(&off_events).is_empty(), "{off_events:?}");
}
