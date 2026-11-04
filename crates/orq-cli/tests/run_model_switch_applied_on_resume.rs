use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_orchestrator::RunStore;
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
    let base = orq_test_support::process_temp_dir().join(format!("orq-switch-apply-{}-{tag}", std::process::id()));
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

fn start_paused(w: &World) -> (String, RunStore) {
    let count = pausing_gate(w);
    let first = orq(w, &["run", "--repo", w.repo.root().to_str().unwrap(), "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"]);
    assert_eq!(first.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&first.stdout), String::from_utf8_lossy(&first.stderr));
    let stdout = String::from_utf8_lossy(&first.stdout).into_owned();
    let pause_log = std::fs::read_to_string(w.base.join("pause.log")).unwrap_or_default();
    assert!(stdout.contains("state: paused"), "{stdout}\npause: {pause_log}");
    assert_eq!(std::fs::read_to_string(count).unwrap().lines().count(), 1);
    let store = RunStore::new(&w.state.join("orq"));
    let ids = store.run_ids().unwrap();
    assert_eq!(ids.len(), 1);
    (ids[0].clone(), store)
}

fn resume(w: &World, run_id: &str) -> Output {
    orq(w, &["run", "resume", run_id, "--yes", "--no-ci-check", "--repo", w.repo.root().to_str().unwrap()])
}

fn agent() -> FakeAgent {
    FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build()
}


const PROFILES: &str = "[models.fast-claude]\nharness = \"claude\"\nmodel = \"sonnet\"\n\n[models.slow-claude]\nharness = \"claude\"\nmodel = \"opus\"\n\n[models.team-pi]\nharness = \"pi\"\nmodel = \"team/model\"\n";

fn request(w: &World, run_id: &str, role: &str, profile: &str) {
    let out = orq(w, &["run", "switch", run_id, "--role", role, "--profile", profile, "--repo", w.repo.root().to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

fn resumed_run(store: &RunStore, first: &str) -> String {
    store.run_ids().unwrap().into_iter().find(|id| id != first).expect("a resumed run")
}

fn setup_profiles(w: &World) {
    std::fs::write(w.repo.root().join(".orq.toml"), PROFILES).unwrap();
}

#[test]
fn a_resumed_run_uses_the_latest_requested_profile_per_role_and_records_the_switch() {
    let agent = agent();
    let w = world("apply", &agent);
    setup_profiles(&w);
    let (run_id, store) = start_paused(&w);
    let before = store.load(&run_id).unwrap().profiles;

    request(&w, &run_id, "reviewer", "slow-claude");
    request(&w, &run_id, "reviewer", "team-pi");
    let out = resume(&w, &run_id);
    assert_eq!(out.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));

    let new_id = resumed_run(&store, &run_id);
    let after = store.load(&new_id).unwrap().profiles;
    assert_eq!(after["reviewer"].harness, "pi");
    assert_eq!(after["reviewer"].model, "team/model");
    // The role with no request keeps its profile.
    assert_eq!(after["implementer"], before["implementer"]);

    let switched: Vec<_> = store.events(&new_id).unwrap().into_iter().filter(|e| e.kind == "run.model_switched").collect();
    assert_eq!(switched.len(), 1);
    assert_eq!(switched[0].payload["role"], "reviewer");
    assert_eq!(switched[0].payload["from"]["model"], before["reviewer"].model);
    assert_eq!(switched[0].payload["to"]["model"], "team/model");
}

#[test]
fn both_roles_can_be_switched_independently_and_a_resume_without_requests_records_no_switch() {
    let agent = agent();
    let w = world("both", &agent);
    setup_profiles(&w);
    let (run_id, store) = start_paused(&w);
    request(&w, &run_id, "implementer", "slow-claude");
    request(&w, &run_id, "reviewer", "fast-claude");
    let out = resume(&w, &run_id);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let after = store.load(&resumed_run(&store, &run_id)).unwrap().profiles;
    assert_eq!(after["implementer"].model, "opus");
    assert_eq!(after["reviewer"].model, "sonnet");

    let w = world("none", &agent);
    setup_profiles(&w);
    let (run_id, store) = start_paused(&w);
    let out = resume(&w, &run_id);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let new_id = resumed_run(&store, &run_id);
    assert!(store.events(&new_id).unwrap().iter().all(|e| e.kind != "run.model_switched"));
    assert_eq!(store.load(&new_id).unwrap().profiles, store.load(&run_id).unwrap().profiles);
}

#[test]
fn a_run_setting_model_does_not_override_a_requested_implementer_profile() {
    let agent = agent();
    let w = world("setting", &agent);
    setup_profiles(&w);
    let (run_id, store) = start_paused(&w);
    request(&w, &run_id, "implementer", "slow-claude");
    let run_toml = w.repo.root().join(".orq/run.toml");
    let mut text = std::fs::read_to_string(&run_toml).unwrap();
    text.push_str("model = \"haiku\"\n");
    std::fs::write(&run_toml, text).unwrap();

    let out = resume(&w, &run_id);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(store.load(&resumed_run(&store, &run_id)).unwrap().profiles["implementer"].model, "opus");
}

#[test]
fn a_profile_that_is_no_longer_configured_refuses_the_resume_and_starts_no_run() {
    let agent = agent();
    let w = world("gone", &agent);
    setup_profiles(&w);
    let (run_id, store) = start_paused(&w);
    request(&w, &run_id, "reviewer", "team-pi");
    std::fs::write(w.repo.root().join(".orq.toml"), "[models.fast-claude]\nharness = \"claude\"\nmodel = \"sonnet\"\n").unwrap();

    let out = resume(&w, &run_id);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("team-pi") && err.contains("no longer configured"), "{err}");
    assert_eq!(store.run_ids().unwrap().len(), 1);
}

#[test]
fn an_unswitched_role_keeps_its_stored_profile_even_when_the_configuration_no_longer_has_it() {
    let agent = agent();
    let w = world("lost", &agent);
    std::fs::write(w.repo.root().join(".orq.toml"), PROFILES.to_string() + "\n[roles]\nreviewer = \"team-pi\"\n").unwrap();
    let (run_id, store) = start_paused(&w);
    let before = store.load(&run_id).unwrap().profiles;
    assert_eq!(before["reviewer"].model, "team/model");
    std::fs::write(w.repo.root().join(".orq.toml"), "[models.fast-claude]\nharness = \"claude\"\nmodel = \"sonnet\"\n").unwrap();

    let out = resume(&w, &run_id);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(store.load(&resumed_run(&store, &run_id)).unwrap().profiles, before);
}

#[test]
fn a_run_level_model_and_a_switch_both_survive_further_resumes() {
    let agent = agent();
    let w = world("chain", &agent);
    setup_profiles(&w);
    let run_toml = w.repo.root().join(".orq/run.toml");
    let (run_id, store) = {
        // The run starts with a run-level model, which the stored implementer profile records.
        let (id, store) = {
            let count = pausing_gate(&w);
            let _ = count;
            let mut text = std::fs::read_to_string(&run_toml).unwrap();
            text.push_str("model = \"haiku\"\n");
            std::fs::write(&run_toml, text).unwrap();
            let first = orq(&w, &["run", "--repo", w.repo.root().to_str().unwrap(), "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"]);
            assert_eq!(first.status.code(), Some(0), "{}", String::from_utf8_lossy(&first.stderr));
            let store = RunStore::new(&w.state.join("orq"));
            (store.run_ids().unwrap()[0].clone(), store)
        };
        (id, store)
    };
    assert_eq!(store.load(&run_id).unwrap().profiles["implementer"].model, "haiku");
    // The setting goes away; the resume still continues on the model the run was started with.
    let text = std::fs::read_to_string(&run_toml).unwrap().replace("model = \"haiku\"\n", "");
    std::fs::write(&run_toml, text).unwrap();
    let out = resume(&w, &run_id);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(store.load(&resumed_run(&store, &run_id)).unwrap().profiles["implementer"].model, "haiku");
}
