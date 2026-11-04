use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_orchestrator::{carry_session_references, session_references, Event, RoleProfile, RunRecord, RunStore};
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
fn a_resume_restores_the_model_the_run_was_started_with_after_the_setting_is_gone() {
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

#[test]
fn a_switch_keeps_the_sessions_already_run_queryable_with_their_harness_and_model() {
    let agent = agent();
    let w = world("sessions", &agent);
    setup_profiles(&w);
    let (run_id, store) = start_paused(&w);
    let had = store.load(&run_id).unwrap().profiles["implementer"].clone();
    let events_before = store.events(&run_id).unwrap().len();

    request(&w, &run_id, "implementer", "slow-claude");
    let out = resume(&w, &run_id);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));

    let new_id = resumed_run(&store, &run_id);
    let sessions = orq_orchestrator::session_references(&store, &new_id).unwrap();
    let first = sessions.iter().find(|s| s.run_id == run_id && s.stage == "implement_and_review").expect("the paused run's implement session");
    assert_eq!((first.role.as_str(), first.harness.as_str(), first.model.as_str()), ("implementer", had.harness.as_str(), had.model.as_str()));
    // Nothing of the paused run's history was deleted or overwritten (only the switch request and the resume were added).
    assert!(store.events(&run_id).unwrap().len() >= events_before);
    assert_eq!(store.load(&run_id).unwrap().profiles["implementer"], had);
}


fn chain_record(id: &str, model: &str) -> RunRecord {
    let mut profiles = BTreeMap::new();
    profiles.insert(
        "implementer".to_string(),
        RoleProfile { harness: "claude".into(), model: model.into(), thinking: None },
    );
    RunRecord {
        run_id: id.into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: "a".repeat(40),
        branch: "orq/x".into(),
        worktree: std::path::PathBuf::from("/tmp/none"),
        repository: String::new(),
        profiles,
    }
}

fn chain_session_started(run: &str, stage: &str, session: Option<&str>) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("{run}-{stage}"),
        run_id: run.into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        kind: "stage.started".into(),
        stage: Some(stage.into()),
        attempt: 1,
        occurred_at: "2026-11-02T10:00:00Z".into(),
        payload: serde_json::json!({ "session_id": session, "log": "/tmp/x.log" }),
    }
}

#[test]
fn sessions_of_every_earlier_run_in_a_chain_stay_queryable_and_history_is_untouched() {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-session-refs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = RunStore::new(&dir);
    for (id, model) in [("run-1", "sonnet"), ("run-2", "opus"), ("run-3", "opus")] {
        store.create(&chain_record(id, model)).unwrap();
    }
    store.append_event(&chain_session_started("run-1", "implement_and_review", Some("sess-1"))).unwrap();
    store.append_event(&chain_session_started("run-2", "gate_fix", None)).unwrap();
    let before = store.events("run-1").unwrap();

    carry_session_references(&store, &store.load("run-2").unwrap(), "run-1").unwrap();
    carry_session_references(&store, &store.load("run-3").unwrap(), "run-2").unwrap();

    let refs = session_references(&store, "run-3").unwrap();
    assert_eq!(refs.len(), 2, "{refs:?}");
    assert_eq!((refs[0].run_id.as_str(), refs[0].stage.as_str(), refs[0].model.as_str(), refs[0].session_id.as_deref()), ("run-1", "implement_and_review", "sonnet", Some("sess-1")));
    assert_eq!((refs[1].run_id.as_str(), refs[1].stage.as_str(), refs[1].model.as_str(), refs[1].session_id.as_deref()), ("run-2", "gate_fix", "opus", None));
    assert_eq!(refs[0].harness, "claude");
    assert_eq!(refs[0].role, "implementer");
    assert_eq!(store.events("run-1").unwrap(), before);
}

#[test]
fn a_carried_history_that_cannot_be_decoded_is_an_error_not_an_empty_history() {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-session-refs-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = RunStore::new(&dir);
    store.create(&chain_record("run-1", "sonnet")).unwrap();
    let mut bad = chain_session_started("run-1", "x", None);
    bad.event_id = "bad".into();
    bad.kind = "run.sessions_carried".into();
    bad.stage = None;
    bad.payload = serde_json::json!({ "sessions": [{ "run_id": "run-0" }] });
    store.append_event(&bad).unwrap();
    assert!(session_references(&store, "run-1").is_err());
    bad.event_id = "bad2".into();
    bad.payload = serde_json::json!({});
    store.append_event(&bad).unwrap();
    assert!(session_references(&store, "run-1").is_err());
}
