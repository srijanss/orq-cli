//! `orq run` uses the agents a project saved in this machine's orq.db: a flag still wins over them,
//! and `reviewers.allowedModels` refuses a project reviewer it does not list before anything starts.
//! `--check` makes every check a launch makes, so it shows the resolution a launch would use.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_core::project_settings::{ProjectAgent, ProjectSettings, SaveOutcome};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

const MACHINE: &str = r#"
[models.opus]
harness = "claude"
model = "opus"

[roles]
implementer = "opus"
"#;

const EXIT_MODEL_SELECTION: i32 = 21;

fn home(tag: &str) -> PathBuf {
    let home = orq_test_support::process_temp_dir().join(format!("orq-run-project-agents-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("config/orq")).unwrap();
    std::fs::write(home.join("config/orq/config.toml"), MACHINE).unwrap();
    home
}

fn save(home: &Path, repo: &Path, settings: &ProjectSettings) {
    let base = home.to_path_buf();
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
    let root = std::fs::canonicalize(repo).unwrap();
    let revision = store.project_settings(&root).unwrap().revision;
    assert!(matches!(store.save_project_settings(&root, revision, settings).unwrap(), SaveOutcome::Saved { .. }));
}

fn check(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(repo)
        .args(["--remote", "origin", "--target", "main", "--feature", "COM-01", "--check"])
        .args(extra)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A repository with one pending feature and an `origin` a run could open its review on.
fn repo() -> GitFixture {
    let repo = GitFixture::new();
    let added = Command::new("git").arg("-C").arg(repo.root()).args(["remote", "add", "origin", "git@github.com:o/r.git"]).status().unwrap();
    assert!(added.success());
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    repo
}

fn agent(harness: &str, model: &str) -> ProjectAgent {
    ProjectAgent { harness: harness.to_string(), model: model.to_string(), effort: None }
}

#[test]
fn a_run_uses_the_project_s_implementer_unless_a_flag_names_one() {
    let repo = repo();
    let home = home("implementer");
    save(&home, repo.root(), &ProjectSettings { implementer: Some(agent("bogus", "m1")), ..ProjectSettings::default() });

    let saved = check(repo.root(), &home, &[]);
    assert_eq!(saved.status.code(), Some(EXIT_MODEL_SELECTION), "the project's implementer was used, not [roles]: {}", text(&saved.stderr));
    assert!(text(&saved.stderr).contains("bogus"), "{}", text(&saved.stderr));

    let flagged = check(repo.root(), &home, &["--implementer", "opus"]);
    assert_eq!(flagged.status.code(), Some(0), "{}", text(&flagged.stderr));
    assert_eq!(text(&flagged.stdout), "state: ready\n");
}

#[test]
fn a_project_reviewer_that_allowed_models_does_not_list_stops_the_run_before_it_starts() {
    let repo = repo();
    std::fs::write(repo.root().join(".tdd-config.json"), r#"{"reviewers":{"allowedModels":["vendor/listed"]}}"#).unwrap();
    let home = home("reviewer");
    save(&home, repo.root(), &ProjectSettings { reviewer: Some(agent("pi", "vendor/project")), ..ProjectSettings::default() });

    let refused = check(repo.root(), &home, &[]);
    assert_eq!(refused.status.code(), Some(EXIT_MODEL_SELECTION), "{}", text(&refused.stderr));
    assert!(text(&refused.stderr).contains("vendor/project"), "{}", text(&refused.stderr));
    assert_eq!(text(&refused.stdout), "", "nothing was reported ready");
}

fn git_ok(dir: &Path, args: &[&str]) {
    let output = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", text(&output.stderr));
}

fn stub(dir: &Path, name: &str, target: &Path) {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\nexec '{}' \"$@\"\n", target.display())).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

#[test]
fn a_run_that_names_no_agents_starts_the_project_s_agents_with_their_effort() {
    use orq_orchestrator::{RoleProfile, RunStore};
    use orq_test_support::{FakeAgent, FakeProvider};
    const PR_URL: &str = "https://github.example/o/r/pull/7";

    let claude = FakeAgent::builder().write_file("claude.txt", "c\n").commit("feat: claude").build();
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git_ok(repo.root(), &["branch", "-M", "main"]);
    git_ok(repo.root(), &["push", "--quiet", "origin", "main"]);
    git_ok(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let home = home("launch");
    let ssh = home.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build();
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    stub(&bin, "claude", claude.path());
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));

    let implementer = ProjectAgent { effort: Some("low".into()), ..agent("claude", "sonnet") };
    let reviewer = ProjectAgent { effort: Some("high".into()), ..agent("pi", "vendor/project") };
    save(&home, repo.root(), &ProjectSettings { implementer: Some(implementer), reviewer: Some(reviewer), ..ProjectSettings::default() });

    let out = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));

    let args = claude.first_run().expect("the implementer was started").args;
    let after = |flag: &str| args.iter().position(|arg| arg == flag).and_then(|at| args.get(at + 1)).cloned();
    assert_eq!((after("--model"), after("--effort")), (Some("sonnet".into()), Some("low".into())), "{args:?}");

    let store = RunStore::new(&home.join("state/orq"));
    let run_id = store.run_ids().unwrap().remove(0);
    let profiles = store.load(&run_id).unwrap().profiles;
    let expected = |harness: &str, model: &str, thinking: &str| RoleProfile { harness: harness.into(), model: model.into(), thinking: Some(thinking.into()) };
    assert_eq!(profiles["implementer"], expected("claude", "sonnet", "low"));
    assert_eq!(profiles["reviewer"], expected("pi", "vendor/project", "high"));
}
