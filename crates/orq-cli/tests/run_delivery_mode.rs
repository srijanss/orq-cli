//! A run records what it does when it finishes, its delivery: `--delivery review|push|merge-local`,
//! else the delivery its project saved in this machine's orq.db, else review. It is read once, when
//! the run starts, so a later change to the project's settings never alters a run in flight, and a
//! resumed run keeps the delivery it started with.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_core::project_settings::{Delivery, ProjectSettings, SaveOutcome};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::RunStore;
use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const PR_URL: &str = "https://github.example/o/r/pull/7";

struct World {
    repo: GitFixture,
    home: PathBuf,
    ssh: PathBuf,
    path: std::ffi::OsString,
    _provider: FakeProvider,
    _agent: FakeAgent,
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

fn executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

fn world(tag: &str) -> World {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let home = orq_test_support::process_temp_dir().join(format!("orq-run-delivery-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("config/orq")).unwrap();
    let ssh = home.join("ssh");
    executable(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n");
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build();
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    executable(&bin.join("claude"), &format!("#!/bin/sh\ncase \"$*\" in *'local gate failed'*) exit 0;; esac\nexec '{}' \"$@\"\n", agent.path().display()));
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(dirs).unwrap();
    World { repo, home, ssh, path, _provider: provider, _agent: agent }
}

fn orq(w: &World, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(args)
        .env("XDG_CONFIG_HOME", w.home.join("config"))
        .env("XDG_DATA_HOME", w.home.join("data"))
        .env("XDG_STATE_HOME", w.home.join("state"))
        .env("XDG_CACHE_HOME", w.home.join("cache"))
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &w.path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn save_delivery(w: &World, delivery: Delivery) {
    let base = w.home.clone();
    let roots = StorageRoots::resolve(&|name| match name {
        "XDG_DATA_HOME" => Some(base.join("data").display().to_string()),
        "XDG_CONFIG_HOME" => Some(base.join("config").display().to_string()),
        "XDG_STATE_HOME" => Some(base.join("state").display().to_string()),
        "XDG_CACHE_HOME" => Some(base.join("cache").display().to_string()),
        _ => None,
    })
    .unwrap();
    roots.create_all().unwrap();
    let mut store = Store::open(&roots.database()).unwrap();
    let root = std::fs::canonicalize(w.repo.root()).unwrap();
    let revision = store.project_settings(&root).unwrap().revision;
    let settings = ProjectSettings { delivery, ..ProjectSettings::default() };
    assert!(matches!(store.save_project_settings(&root, revision, &settings).unwrap(), SaveOutcome::Saved { .. }));
}

fn run(w: &World, extra: &[&str]) -> Output {
    let repo = w.repo.root().to_str().unwrap().to_string();
    let mut args = vec!["run", "--repo", &repo, "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"];
    args.extend_from_slice(extra);
    orq(w, &args)
}

fn store(w: &World) -> RunStore {
    RunStore::new(&w.home.join("state/orq"))
}

fn only_run_delivery(w: &World, extra: &[&str]) -> Delivery {
    let out = run(w, extra);
    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    let ids = store(w).run_ids().unwrap();
    assert_eq!(ids.len(), 1);
    store(w).load(&ids[0]).unwrap().delivery
}

#[test]
fn a_run_records_the_flag_s_delivery_else_the_project_s_else_review() {
    assert_eq!(only_run_delivery(&world("default"), &[]), Delivery::Review, "no record: review");

    let saved = world("saved");
    save_delivery(&saved, Delivery::Push);
    assert_eq!(only_run_delivery(&saved, &[]), Delivery::Push, "the project's delivery");

    let flagged = world("flagged");
    save_delivery(&flagged, Delivery::Push);
    assert_eq!(only_run_delivery(&flagged, &["--delivery", "merge-local"]), Delivery::MergeLocal, "the flag wins");
}

#[test]
fn a_delivery_flag_that_names_no_mode_is_refused_before_anything_starts() {
    let w = world("refused");
    for bad in ["ship", "merge_local", ""] {
        let out = run(&w, &["--delivery", bad, "--check"]);
        assert_eq!(out.status.code(), Some(2), "{bad:?}: {}", text(&out.stderr));
        assert!(text(&out.stderr).contains("--delivery"), "{bad:?}: {}", text(&out.stderr));
    }
    assert!(store(&w).run_ids().unwrap_or_default().is_empty(), "nothing was recorded");
}

/// A gate that asks the run it belongs to to pause at its next boundary.
fn pausing_gate(w: &World) {
    let script = w.home.join("gate.sh");
    let state = w.home.join("state");
    executable(
        &script,
        &format!(
            "#!/bin/sh\n[ -e '{done}' ] && exit 0\ntouch '{done}'\nXDG_STATE_HOME='{state}' '{orq}' run pause \"$(ls '{state}/orq/runs')\" >/dev/null 2>&1\nexit 0\n",
            done = w.home.join("paused-once").display(),
            orq = env!("CARGO_BIN_EXE_orq"),
            state = state.display(),
        ),
    );
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), format!("gate = [\"{}\"]\nexplain_failures = false\n", script.display())).unwrap();
}

#[test]
fn a_resumed_run_keeps_the_delivery_it_started_with() {
    let w = world("resume");
    pausing_gate(&w);
    save_delivery(&w, Delivery::Push);
    let first = run(&w, &[]);
    assert!(text(&first.stdout).contains("state: paused"), "{}\n{}", text(&first.stdout), text(&first.stderr));
    let paused = store(&w).run_ids().unwrap().remove(0);

    // The project's setting changes while the run is paused.
    let base = w.home.clone();
    let roots = StorageRoots::resolve(&|name| match name {
        "XDG_DATA_HOME" => Some(base.join("data").display().to_string()),
        _ => Some(base.join("other").display().to_string()),
    })
    .unwrap();
    let mut db = Store::open(&roots.database()).unwrap();
    let root = std::fs::canonicalize(w.repo.root()).unwrap();
    let revision = db.project_settings(&root).unwrap().revision;
    db.save_project_settings(&root, revision, &ProjectSettings { delivery: Delivery::MergeLocal, ..ProjectSettings::default() }).unwrap();

    let repo = w.repo.root().to_str().unwrap().to_string();
    let resumed = orq(&w, &["run", "resume", &paused, "--yes", "--no-ci-check", "--repo", &repo]);
    assert_eq!(resumed.status.code(), Some(0), "{}\n{}", text(&resumed.stdout), text(&resumed.stderr));
    let new_id = store(&w).run_ids().unwrap().into_iter().find(|id| *id != paused).expect("a resumed run");
    assert_eq!(store(&w).load(&new_id).unwrap().delivery, Delivery::Push, "kept from the paused run");
}
