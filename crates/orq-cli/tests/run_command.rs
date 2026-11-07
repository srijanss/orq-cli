use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

/// Exit codes of `orq run`, one per outcome.
const EXIT_INELIGIBLE: i32 = 20;
const EXIT_MODEL_SELECTION: i32 = 21;
const EXIT_BUSY: i32 = 22;
const EXIT_BLOCKED: i32 = 30;
const EXIT_FAILED: i32 = 31;

struct Env {
    state: PathBuf,
    config: PathBuf,
    data: PathBuf,
}

fn env(tag: &str) -> Env {
    let root = orq_test_support::process_temp_dir().join(format!("orq-run-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config/orq")).unwrap();
    std::fs::create_dir_all(root.join("state")).unwrap();
    Env { state: root.join("state"), config: root.join("config"), data: root.join("data") }
}

fn run(repo: &Path, env: &Env, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(repo)
        .args(["--remote", "origin", "--target", "main"])
        .args(extra)
        .env("XDG_CONFIG_HOME", &env.config)
        .env("XDG_STATE_HOME", &env.state)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn state_is_empty(env: &Env) -> bool {
    std::fs::read_dir(&env.state).map(|mut entries| entries.next().is_none()).unwrap_or(true)
}

#[test]
fn a_feature_that_may_not_start_is_refused_before_anything_is_created() {
    let cases: [(&str, Vec<LedgerEntry>, &str, &str); 3] = [
        ("missing", vec![LedgerEntry::new("other", "pending")], "COM-01", "not found"),
        ("done", vec![LedgerEntry::new("COM-01", "completed")], "COM-01", "completed"),
        (
            "unmet",
            vec![LedgerEntry::new("base", "pending"), LedgerEntry::new("COM-01", "pending").depends_on(&["base"])],
            "COM-01",
            "base",
        ),
    ];
    for (tag, entries, feature, expected) in cases {
        let repo = GitFixture::new();
        write_ledger(repo.root(), &entries);
        let env = env(tag);

        let output = run(repo.root(), &env, &["--feature", feature]);

        assert_eq!(output.status.code(), Some(EXIT_INELIGIBLE), "{tag}: {}", stderr(&output));
        assert!(stderr(&output).contains(expected), "{tag}: {}", stderr(&output));
        assert!(output.stdout.is_empty(), "{tag}: nothing is printed on stdout");
        assert!(state_is_empty(&env), "{tag}: no run was recorded");
        let worktrees = Command::new("git").arg("-C").arg(repo.root()).args(["worktree", "list"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&worktrees.stdout).lines().count(), 1, "{tag}: no worktree was made");
    }
}

#[test]
fn a_profile_that_is_not_defined_is_refused_before_anything_is_created() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let env = env("profile");

    let output = run(repo.root(), &env, &["--feature", "COM-01", "--implementer", "nope"]);

    assert_eq!(output.status.code(), Some(EXIT_MODEL_SELECTION), "{}", stderr(&output));
    assert!(stderr(&output).contains("nope"), "{}", stderr(&output));
    assert!(state_is_empty(&env));
}

#[test]
fn a_bad_settings_file_is_refused_by_name_before_a_run_is_recorded() {
    let repo = GitFixture::new();
    repo.add_bare_remote();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    std::fs::write(repo.root().join(".orq/run.toml"), "gate = \"make check\"\n").unwrap();
    let env = env("badsettings");

    let output = run(repo.root(), &env, &["--feature", "COM-01"]);

    assert!(!output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains(".orq/run.toml") && stderr(&output).contains("gate"), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
    assert!(state_is_empty(&env), "no run was recorded");
}

#[test]
fn a_bad_settings_file_stops_a_detached_launch_too() {
    let repo = GitFixture::new();
    repo.add_bare_remote();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    std::fs::write(repo.root().join(".orq/run.toml"), "ci_timeout_secs = 0\n").unwrap();
    let env = env("baddetach");

    let output = run(repo.root(), &env, &["--feature", "COM-01", "--detach"]);

    assert!(!output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains("ci_timeout_secs"), "{}", stderr(&output));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("state: launched"));
    assert!(state_is_empty(&env), "no log directory or runner was created");
}

#[test]
fn a_configured_gate_runs_and_a_failing_one_stops_the_run_before_the_push() {
    let w = world("cfggate");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), "gate = [\"false\"]\n").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_ne!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert!(!stdout.contains("state: review_ready"), "{stdout}");
    assert!(!git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "nothing was pushed");
}

#[test]
fn a_run_names_the_script_file_its_gate_came_from() {
    let w = world("scriptgatenotice");
    std::fs::write(w.repo.root().join(".git/orchestrate.env"), "GATE_CMD=true\n").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(0), "{err}");
    assert!(err.contains("GATE_CMD") && err.contains("orchestrate.env"), "{err}");
}

/// The target branch gets a new commit while the run sets up, so the commit the run started from
/// and the one it rebases onto differ. Returns the new commit.
fn target_moves_during_setup(w: &World) -> String {
    let other = orq_test_support::process_temp_dir().join(format!("orq-run-other-{}-{}", std::process::id(), w.tag));
    let _ = std::fs::remove_dir_all(&other);
    git(w.repo.root(), &["clone", "--quiet", "--branch", "main", &w.remote.display().to_string(), &other.display().to_string()]);
    git(&other, &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "--quiet", "--allow-empty", "-m", "moved"]);
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(
        w.repo.root().join(".orq/run.toml"),
        format!("setup = [[\"git\", \"-C\", \"{}\", \"push\", \"--quiet\", \"origin\", \"main\"]]\n", other.display()),
    )
    .unwrap();
    git(&other, &["rev-parse", "HEAD"]).trim().to_string()
}

/// Every run of the gate leaves its `GATE_BASE` as a line in the returned file.
fn gate_records_its_base(w: &World) -> PathBuf {
    let seen = orq_test_support::process_temp_dir().join(format!("orq-run-gate-base-{}-{}", std::process::id(), w.tag));
    let _ = std::fs::remove_file(&seen);
    std::fs::write(w.repo.root().join(".git/orchestrate.env"), format!("GATE_CMD='echo \"$GATE_BASE\" >> {}'\n", seen.display())).unwrap();
    seen
}

/// The script told its gate what the branch sits on, and every run of the gate is told the same:
/// also the one after a CI fix, before the fix is pushed.
#[test]
fn every_gate_of_a_run_is_told_the_commit_its_branch_was_rebased_onto() {
    let w = world("gatebase");
    let moved = target_moves_during_setup(&w);
    let seen = gate_records_its_base(&w);
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let red = "{\"total_count\":1,\"check_runs\":[{\"name\":\"test\",\"status\":\"completed\",\"conclusion\":\"failure\"}]}\n";
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .reply(&["api"], red)
        .build();

    let output = run_with(&w, &agent, &provider, &[]);

    let seen = std::fs::read_to_string(&seen).unwrap_or_default();
    let bases: Vec<&str> = seen.lines().collect();
    assert!(bases.len() >= 2, "the gate ran again after the CI fix: {bases:?}\n{}", stderr(&output));
    assert!(bases.iter().all(|base| *base == moved), "expected {moved}: {bases:?}");
}

/// A branch that was not rebased still sits on the commit it started from, whatever the target did.
#[test]
fn a_gate_of_a_run_that_does_not_rebase_is_told_the_commit_it_started_from() {
    let w = world("gatebasenorebase");
    let started = git(&w.remote, &["rev-parse", "main"]).trim().to_string();
    let moved = target_moves_during_setup(&w);
    let seen = gate_records_its_base(&w);
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check", "--no-rebase"]);

    assert_ne!(started, moved);
    assert_eq!(std::fs::read_to_string(&seen).unwrap_or_default().trim(), started, "{}", stderr(&output));
}

/// A project set up for the old script keeps its gate: the shell command is what runs, and the run says so.
#[test]
fn the_gate_command_of_the_old_script_settings_runs_when_nothing_else_names_a_gate() {
    let w = world("scriptgate");
    std::fs::write(
        w.repo.root().join(".git/orchestrate.env"),
        "GATE_CMD='rc=0; true || rc=1; false || rc=1; exit $rc'\n",
    )
    .unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_ne!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert!(!git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "nothing was pushed");
    assert!(stderr(&output).contains("GATE_CMD"), "the run says where its gate came from: {}", stderr(&output));
}

#[test]
fn auto_exit_false_in_the_settings_file_asks_for_no_marker_and_keeps_the_sessions_own_exit_status() {
    let w = world("noautoexit");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), "auto_exit = false\n").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").exit_code(3).build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    let args = agent.first_run().expect("the agent ran").args;
    assert!(!args.last().unwrap().contains("implement.done"), "{args:?}");
    assert_ne!(output.status.code(), Some(0), "the session's failing exit status stops the run: {}", stderr(&output));
}

#[test]
fn extra_context_reaches_the_agent_inside_the_one_prompt_argument() {
    let w = world("ctx");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let _ = run_with(&w, &agent, &github(), &["--no-ci-check", "--context", "keep the API stable --no-gate"]);

    let args = agent.first_run().expect("the agent ran").args;
    let prompt = args.last().unwrap();
    assert!(prompt.starts_with("/implement-and-review COM-01"), "{prompt}");
    assert!(prompt.contains("Extra context:\nkeep the API stable --no-gate"), "{prompt}");
    assert!(prompt.contains("implement.done"), "the session is asked for its exit marker: {prompt}");
}

#[test]
fn bad_extra_context_is_refused_before_a_run_is_recorded() {
    let repo = GitFixture::new();
    repo.add_bare_remote();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let env = env("badctx");
    let long = "x".repeat(4001);

    for bad in ["bell\u{7}", long.as_str()] {
        let output = run(repo.root(), &env, &["--feature", "COM-01", "--context", bad]);
        assert!(!output.status.success(), "{}", stderr(&output));
        assert!(stderr(&output).contains("context"), "{}", stderr(&output));
        assert!(state_is_empty(&env), "no run was recorded");
    }
}

#[test]
fn an_issue_id_closes_the_issue_from_the_review_description() {
    let w = world("issue");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();

    let _ = run_with(&w, &agent, &provider, &["--no-ci-check", "--issue", "#42"]);

    let create = provider.calls().into_iter().find(|c| c.args.starts_with(&["pr".to_string(), "create".to_string()])).expect("a review was created");
    assert!(create.args.iter().any(|a| a.ends_with("\n\nCloses #42")), "{:?}", create.args);
}

#[test]
fn a_bad_issue_id_is_refused_before_a_run_is_recorded() {
    let repo = GitFixture::new();
    repo.add_bare_remote();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let env = env("badissue");

    for flags in [&["--issue", "abc"][..], &["--issue", "4 2", "--detach"][..]] {
        let mut args = vec!["--feature", "COM-01"];
        args.extend_from_slice(flags);
        let output = run(repo.root(), &env, &args);
        assert!(!output.status.success(), "{}", stderr(&output));
        assert!(stderr(&output).contains("issue"), "{}", stderr(&output));
        assert!(state_is_empty(&env), "no run was recorded");
    }
}

#[test]
fn an_assignee_and_a_reviewer_are_requested_on_the_new_review() {
    let w = world("people");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();

    let _ = run_with(&w, &agent, &provider, &["--no-ci-check", "--mr-assignee", "alice", "--mr-reviewer", "bob"]);

    let create = provider.calls().into_iter().find(|c| c.args.starts_with(&["pr".to_string(), "create".to_string()])).expect("a review was created");
    let after = |flag: &str| create.args.windows(2).find(|p| p[0] == flag).map(|p| p[1].clone());
    assert_eq!((after("--assignee").as_deref(), after("--reviewer").as_deref()), (Some("alice"), Some("bob")));
}

#[test]
fn a_flag_replaces_only_its_own_configured_person_and_a_blank_one_clears_it() {
    let create_args = |tag: &str, extra: &[&str]| {
        let w = world(tag);
        std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
        std::fs::write(w.repo.root().join(".orq/run.toml"), "mr_assignee = \"alice\"\nmr_reviewer = \"bob\"\n").unwrap();
        let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
        let provider = github();
        let mut flags = vec!["--no-ci-check"];
        flags.extend_from_slice(extra);
        let _ = run_with(&w, &agent, &provider, &flags);
        let create = provider.calls().into_iter().find(|c| c.args.starts_with(&["pr".to_string(), "create".to_string()])).expect("a review was created");
        let after = |flag: &str| create.args.windows(2).find(|p| p[0] == flag).map(|p| p[1].clone());
        (after("--assignee"), after("--reviewer"))
    };

    assert_eq!(create_args("people-cfg", &[]), (Some("alice".to_string()), Some("bob".to_string())));
    assert_eq!(create_args("people-one", &["--mr-reviewer", "carol"]), (Some("alice".to_string()), Some("carol".to_string())));
    assert_eq!(create_args("people-blank", &["--mr-assignee", ""]), (None, Some("bob".to_string())));
}

#[test]
fn a_bad_assignee_or_reviewer_is_refused_before_a_run_is_recorded() {
    let repo = GitFixture::new();
    repo.add_bare_remote();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let env = env("badpeople");

    for flags in [&["--mr-assignee", "-x"][..], &["--mr-reviewer", "a b", "--detach"][..]] {
        let mut args = vec!["--feature", "COM-01"];
        args.extend_from_slice(flags);
        let output = run(repo.root(), &env, &args);
        assert!(!output.status.success(), "{}", stderr(&output));
        assert!(stderr(&output).contains("mr-"), "{}", stderr(&output));
        assert!(state_is_empty(&env), "no run was recorded");
    }
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
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A directory holding a `claude` that runs the fake agent, next to the fake `gh`.
fn path_with(agent: &FakeAgent, provider: &FakeProvider, tag: &str) -> std::ffi::OsString {
    let bin = orq_test_support::process_temp_dir().join(format!("orq-run-bin-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&bin);
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, format!("#!/bin/sh\nexec '{}' \"$@\"\n", agent.path().display())).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(dirs).unwrap()
}

struct World {
    repo: GitFixture,
    remote: PathBuf,
    env: Env,
    ssh: PathBuf,
    tag: String,
}

/// A repository with a pushed `main`, a pending COM-01, and a remote the review stage can name.
fn world(tag: &str) -> World {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    // An scp-style URL the review stage can name, reached through a shim that runs the
    // remote command locally instead of over ssh.
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let ssh = orq_test_support::process_temp_dir().join(format!("orq-run-ssh-{}-{tag}", std::process::id()));
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    World { repo, remote, env: env(tag), ssh, tag: tag.to_string() }
}

const PR_URL: &str = "https://github.example/o/r/pull/7";

fn github() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build()
}

fn run_with(w: &World, agent: &FakeAgent, provider: &FakeProvider, extra: &[&str]) -> Output {
    run_selecting(w, agent, provider, &["--feature", "COM-01"], extra)
}

/// As `run_with`, but the feature is chosen by `selection` (`--feature COM-01`, `--group COM-01`, ...).
fn run_selecting(w: &World, agent: &FakeAgent, provider: &FakeProvider, selection: &[&str], extra: &[&str]) -> Output {
    run_command(w, agent, provider, selection, extra).output().unwrap()
}

fn run_command(w: &World, agent: &FakeAgent, provider: &FakeProvider, selection: &[&str], extra: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    command
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(selection)
        .args(["--remote", "origin", "--target", "main"])
        .args(extra)
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("XDG_DATA_HOME", &w.env.data)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", path_with(agent, provider, &w.tag))
        .env_remove("HERDR_ACTIVE_PANE_CWD");
    command
}

/// What the run noted about its review report.
fn report_log(w: &World, run_id: &str) -> String {
    std::fs::read_to_string(w.env.state.join("orq/logs").join(run_id).join("review-report.log")).unwrap_or_default()
}

/// The run id `orq run` printed.
fn printed_run_id(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().find_map(|line| line.strip_prefix("run: "));
    line.unwrap_or_else(|| panic!("no run id in: {stdout}\n{}", stderr(output))).to_string()
}

/// The `(feature, head_sha)` rows of the review index for `run_id`.
fn review_rows(w: &World, run_id: &str) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open(w.env.data.join("orq/orq.db")).unwrap();
    let mut rows = conn.prepare("SELECT feature, head_sha FROM reviews WHERE run_id = ?1 ORDER BY recorded_at").unwrap();
    rows.query_map([run_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().map(Result::unwrap).collect()
}

fn stored_report(w: &World, run_id: &str, head_sha: &str) -> serde_json::Value {
    let path = w.env.data.join("orq/runs").join(run_id).join("review").join(head_sha).join("review.json");
    serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))).unwrap()
}

/// The Reviews tab reads what the run itself published. The model summary is left to
/// `orq review build`: a run does not wait on, or pay for, a session it does not need.
#[test]
fn a_finished_run_leaves_its_review_report_without_the_model_summary() {
    let w = world("report");
    std::fs::write(w.env.config.join("orq/config.toml"), "[review_summary]\nenabled = true\nmodel = \"haiku\"\n").unwrap();
    let base = git(w.repo.root(), &["rev-parse", "HEAD"]).trim().to_string();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    let run_id = printed_run_id(&output);
    let head = git(&w.remote, &["rev-parse", "fix--COM-01"]).trim().to_string();
    assert_eq!(review_rows(&w, &run_id), vec![("COM-01".to_string(), head.clone())]);
    let report = stored_report(&w, &run_id, &head);
    assert_eq!(report["base_sha"], base.as_str(), "{report}");
    assert_eq!(report["head_sha"], head.as_str(), "{report}");
    assert!(report.get("generated_summary").is_none(), "{report}");
    assert!(w.env.data.join("orq/runs").join(&run_id).join("review").join(&head).join("review.md").exists());
}

/// The diff is `base..head`: measured from the commit the run started on, a rebased branch would
/// claim the target's newer commits as its own changes.
#[test]
fn the_review_report_of_a_rebased_run_starts_at_the_commit_it_was_rebased_onto() {
    let w = world("reportrebase");
    let moved = target_moves_during_setup(&w);
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    let run_id = printed_run_id(&output);
    let head = git(&w.remote, &["rev-parse", "fix--COM-01"]).trim().to_string();
    assert_eq!(stored_report(&w, &run_id, &head)["base_sha"], moved.as_str(), "{}", stderr(&output));
}

/// The report is a convenience: a data directory it cannot be written to costs the run nothing.
#[test]
fn a_review_report_that_cannot_be_built_is_noted_and_the_run_still_finishes() {
    let w = world("reportfails");
    let not_a_directory = w.env.config.join("not-a-directory");
    std::fs::write(&not_a_directory, "").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_command(&w, &agent, &github(), &["--feature", "COM-01"], &["--no-ci-check"])
        .env("XDG_DATA_HOME", &not_a_directory)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: review_ready"), "{stdout}");
    let log = report_log(&w, &printed_run_id(&output));
    assert!(log.contains("no report for"), "{log}");
}

/// Reading the diff hangs: the run moves on and says so.
#[test]
fn a_slow_review_report_does_not_hold_up_the_run() {
    let w = world("reportslow");
    let shims = orq_test_support::process_temp_dir().join(format!("orq-run-slow-git-{}", std::process::id()));
    std::fs::create_dir_all(&shims).unwrap();
    let real_git = String::from_utf8(Command::new("sh").args(["-c", "command -v git"]).output().unwrap().stdout).unwrap();
    std::fs::write(shims.join("git"), format!("#!/bin/sh\ncase \"$*\" in *--numstat*) sleep 10;; esac\nexec {} \"$@\"\n", real_git.trim())).unwrap();
    std::fs::set_permissions(shims.join("git"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let path = format!("{}:{}", shims.display(), path_with(&agent, &provider, &w.tag).display());
    let started = std::time::Instant::now();

    let output = run_command(&w, &agent, &provider, &["--feature", "COM-01"], &["--no-ci-check"])
        .env("PATH", path)
        .env("ORQ_REVIEW_BUILD_TIMEOUT_MS", "100")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "the run waited for the report: {:?}", started.elapsed());
    let log = report_log(&w, &printed_run_id(&output));
    assert!(log.contains("did not finish in time"), "{log}");
}

/// The Reviews tab shows what the branch holds now, so the commit a CI fix pushed gets a report too.
#[test]
fn the_commit_a_ci_fix_pushes_gets_its_own_review_report() {
    let w = world("reportfix");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let red = "{\"total_count\":1,\"check_runs\":[{\"name\":\"test\",\"status\":\"completed\",\"conclusion\":\"failure\"}]}\n";
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .reply(&["api"], red)
        .build();
    // Every fix session commits something new; every other session is the plain fake agent.
    let shims = orq_test_support::process_temp_dir().join(format!("orq-run-fixing-claude-{}", std::process::id()));
    std::fs::create_dir_all(&shims).unwrap();
    std::fs::write(
        shims.join("claude"),
        format!(
            "#!/bin/sh\ncase \"$*\" in *'CI failed on the pushed branch'*)\n  echo fixed > \"fix-$$.txt\"\n  git add -A\n  exec git -c user.name=Fixer -c user.email=fixer@example.invalid commit -q -m fix\nesac\nexec '{}' \"$@\"\n",
            agent.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(shims.join("claude"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", shims.display(), path_with(&agent, &provider, &w.tag).display());

    let output = run_command(&w, &agent, &provider, &["--feature", "COM-01"], &[]).env("PATH", path).output().unwrap();

    let run_id = printed_run_id(&output);
    let pushed = git(&w.remote, &["rev-list", "--reverse", "main..fix--COM-01"]);
    let pushed: Vec<&str> = pushed.lines().collect();
    assert!(pushed.len() > 1, "a fix was pushed: {pushed:?}\n{}", stderr(&output));
    for head in pushed {
        assert_eq!(stored_report(&w, &run_id, head)["head_sha"], head);
    }
}

#[test]
fn a_run_that_succeeds_ends_at_review_ready_and_prints_the_run_id_state_and_url() {
    let w = world("happy");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("run: "), "{stdout}");
    assert!(stdout.contains("state: review_ready"), "{stdout}");
    assert!(stdout.contains(&format!("review: {PR_URL}")), "{stdout}");
    assert!(git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "the run branch was pushed");
    assert_eq!(git(w.repo.root(), &["worktree", "list"]).lines().count(), 1, "the worktree was cleaned up");
}

#[test]
fn a_foreground_run_records_its_owner_so_a_killed_one_can_be_recovered() {
    let w = world("foreground-owner");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    let run_id = stdout.lines().find_map(|l| l.strip_prefix("run: ")).expect(&stdout).to_string();
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    let launched = store.events(&run_id).unwrap().into_iter().find(|e| e.kind == "run.launched");
    let launched = launched.expect("a foreground run records run.launched too");
    assert!(launched.payload.get("pid").and_then(|p| p.as_u64()).is_some(), "{:?}", launched.payload);
    assert!(launched.payload.get("log").is_none(), "a foreground run has no runner log to name: {:?}", launched.payload);
}

/// Every agent invocation (the first and the latest) is the PR-description drafter, never an implement session.
fn assert_only_drafting(agent: &FakeAgent, what: &str) {
    for run in [agent.first_run(), agent.last_run()].into_iter().flatten() {
        assert!(run.args.iter().any(|a| a.starts_with("Write a pull request title")), "{what}: {:?}", run.args);
    }
}

#[test]
fn a_foreground_run_killed_mid_flight_is_recognised_as_interrupted_and_resumes() {
    let w = world("foreground-killed");
    let slow_bin = orq_test_support::process_temp_dir().join(format!("orq-run-killed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&slow_bin);
    std::fs::create_dir_all(&slow_bin).unwrap();
    let claude = slow_bin.join("claude");
    let ready = slow_bin.join("agent-started");
    std::fs::write(&claude, format!("#!/bin/sh\ntouch {}\nexec sleep 30\n", ready.display())).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![slow_bin];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut runner = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    let launched = || {
        store.run_ids().unwrap_or_default().into_iter().find_map(|id| {
            let events = store.events(&id).ok()?;
            events.into_iter().find(|e| e.kind == "run.launched").map(|e| (id, e))
        })
    };
    wait_until("the foreground run to record its owner", || launched().is_some());
    wait_until("the implement agent to be running", || ready.exists());
    let (run_id, owner) = launched().unwrap();
    assert_eq!(owner.payload["pid"].as_u64(), Some(u64::from(runner.id())), "the owner is the runner itself");
    let state = |id: &str| orq_orchestrator::summarize_run(&store.events(id).unwrap()).phase;
    assert_eq!(state(&run_id), orq_orchestrator::RunPhase::Running, "alive and running");

    runner.kill().unwrap();
    runner.wait().unwrap();

    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let output = run_with(&w, &agent, &github(), &["--no-ci-check", "--resume"]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: review_ready"), "{stdout}");
    let events = store.events(&run_id).unwrap();
    let failed = events.iter().find(|e| e.kind == "run.failed").expect("the killed run was marked interrupted");
    assert!(failed.payload["reason"].as_str().unwrap().contains("interrupted"), "{:?}", failed.payload);
}

#[test]
fn the_runs_listing_shows_a_killed_run_as_failed_not_running() {
    let w = world("listing-killed");
    let slow_bin = orq_test_support::process_temp_dir().join(format!("orq-list-killed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&slow_bin);
    std::fs::create_dir_all(&slow_bin).unwrap();
    let claude = slow_bin.join("claude");
    let ready = slow_bin.join("agent-started");
    std::fs::write(&claude, format!("#!/bin/sh\ntouch {}\nexec sleep 30\n", ready.display())).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![slow_bin];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut runner = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the implement agent to be running", || ready.exists());
    runner.kill().unwrap();
    runner.wait().unwrap();

    let listing = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["runs", "--repo"])
        .arg(w.repo.root())
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&listing.stdout).into_owned();
    assert_eq!(listing.status.code(), Some(0), "{}", stderr(&listing));
    assert!(stdout.contains("failed"), "{stdout}");
    assert!(!stdout.contains("running"), "{stdout}");
}

#[test]
fn the_runs_listing_leaves_a_live_run_alone_and_records_a_dead_pause_requested_run_failed_once() {
    let w = world("listing-edges");
    let slow_bin = orq_test_support::process_temp_dir().join(format!("orq-list-edges-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&slow_bin);
    std::fs::create_dir_all(&slow_bin).unwrap();
    let claude = slow_bin.join("claude");
    let ready = slow_bin.join("agent-started");
    std::fs::write(&claude, format!("#!/bin/sh\ntouch '{}'\nexec sleep 30\n", ready.display())).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![slow_bin];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut runner = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the implement agent to be running", || ready.exists());
    let list = |extra: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_orq"))
            .args(["runs", "--repo"])
            .arg(w.repo.root())
            .args(extra)
            .env("XDG_CONFIG_HOME", &w.env.config)
            .env("XDG_STATE_HOME", &w.env.state)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    let run_id = store.run_ids().unwrap().into_iter().next().expect("the run was recorded");

    let alive = list(&[]);
    assert!(alive.contains("running") && !alive.contains("failed"), "a live run is left alone: {alive}");

    orq_orchestrator::request_pause(&store, &run_id).unwrap();
    runner.kill().unwrap();
    runner.wait().unwrap();

    let json = list(&["--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["runs"][0]["phase"], "failed", "{json}");
    let _ = list(&[]);
    let failures = store.events(&run_id).unwrap().iter().filter(|e| e.kind == "run.failed").count();
    assert_eq!(failures, 1, "listing again does not record the failure twice");
}

#[test]
fn the_runs_listing_keeps_a_paused_run_paused_even_though_its_process_is_gone() {
    let w = world("listing-paused");
    let slow_bin = orq_test_support::process_temp_dir().join(format!("orq-list-paused-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&slow_bin);
    std::fs::create_dir_all(&slow_bin).unwrap();
    let claude = slow_bin.join("claude");
    let ready = slow_bin.join("agent-started");
    std::fs::write(&claude, format!("#!/bin/sh\ntouch '{}'\nexec sleep 30\n", ready.display())).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![slow_bin];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut runner = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the implement agent to be running", || ready.exists());
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    let run_id = store.run_ids().unwrap().into_iter().next().expect("the run was recorded");
    orq_orchestrator::request_pause(&store, &run_id).unwrap();
    let requested = store.events(&run_id).unwrap().into_iter().find(|e| e.kind == "run.pause_requested").unwrap();
    store
        .append_event(&orq_orchestrator::Event {
            event_id: format!("{}-paused", requested.event_id),
            kind: "run.paused".to_string(),
            payload: serde_json::json!({}),
            ..requested
        })
        .unwrap();
    runner.kill().unwrap();
    runner.wait().unwrap();

    for extra in [&[][..], &["--json"][..]] {
        let out = Command::new(env!("CARGO_BIN_EXE_orq"))
            .args(["runs", "--repo"])
            .arg(w.repo.root())
            .args(extra)
            .env("XDG_CONFIG_HOME", &w.env.config)
            .env("XDG_STATE_HOME", &w.env.state)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        assert!(stdout.contains("paused") && !stdout.contains("failed"), "{stdout}");
    }
    assert!(store.events(&run_id).unwrap().iter().all(|e| e.kind != "run.failed"), "no failure was recorded");
}

#[test]
fn the_runs_listing_says_so_when_a_dead_run_cannot_be_recorded_as_failed() {
    let w = world("listing-unwritable");
    let slow_bin = orq_test_support::process_temp_dir().join(format!("orq-list-unwritable-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&slow_bin);
    std::fs::create_dir_all(&slow_bin).unwrap();
    let claude = slow_bin.join("claude");
    let ready = slow_bin.join("agent-started");
    std::fs::write(&claude, format!("#!/bin/sh\ntouch '{}'\nexec sleep 30\n", ready.display())).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![slow_bin];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut runner = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the implement agent to be running", || ready.exists());
    runner.kill().unwrap();
    runner.wait().unwrap();
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    let run_id = store.run_ids().unwrap().into_iter().next().expect("the run was recorded");
    let log = w.env.state.join("orq").join("runs").join(&run_id).join("events.jsonl");
    std::fs::set_permissions(&log, std::os::unix::fs::PermissionsExt::from_mode(0o444)).unwrap();
    if std::fs::OpenOptions::new().append(true).open(&log).is_ok() {
        // A privileged user ignores the file mode, so the write cannot be made to fail here.
        eprintln!("skipped: the event log is still writable (running with write privileges)");
        return;
    }

    let listing = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["runs", "--repo"])
        .arg(w.repo.root())
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .output()
        .unwrap();
    assert_eq!(listing.status.code(), Some(0), "{}", stderr(&listing));
    let warning = stderr(&listing);
    assert!(warning.contains(&run_id), "the warning names the run: {warning}");
}

#[test]
fn a_run_that_failed_after_opening_the_review_resumes_without_repeating_the_agent_or_the_review() {
    let w = world("no-replay");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let red = "{\"total_count\":1,\"check_runs\":[{\"name\":\"test\",\"status\":\"completed\",\"conclusion\":\"failure\"}]}\n";
    let first = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .reply(&["api"], red)
        .build();
    let output = run_with(&w, &agent, &first, &[]);
    assert_eq!(output.status.code(), Some(EXIT_FAILED), "{}", stderr(&output));
    assert!(first.calls().iter().any(|c| c.args.starts_with(&["pr".into(), "create".into()])), "the first run opened the review");
    let pushed = git(&w.remote, &["rev-parse", "fix--COM-01"]);

    // The review now exists on the forge; a second agent and provider must show nothing is redone.
    let second_agent = FakeAgent::builder().write_file("again.txt", "x\n").commit("feat: again").build();
    let second = FakeProvider::builder()
        .reply(&["pr", "list"], &format!("[{{\"number\":7,\"url\":\"{PR_URL}\",\"state\":\"OPEN\",\"isCrossRepository\":false}}]\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build();
    let output = run_with(&w, &second_agent, &second, &["--no-ci-check", "--resume"]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert_only_drafting(&second_agent, "the finished implement session is not started again");
    assert!(!w.repo.root().join("again.txt").exists(), "no new work was committed");
    assert!(!second.calls().iter().any(|c| c.args.starts_with(&["pr".into(), "create".into()])), "no second review: {:?}", second.calls());
    assert_eq!(git(&w.remote, &["rev-parse", "fix--COM-01"]), pushed, "the pushed branch is untouched");
    assert!(stdout.contains(PR_URL), "{stdout}");
}

#[test]
fn a_run_that_failed_between_the_push_and_the_review_resumes_to_open_it_once_without_redoing_the_work() {
    let w = world("push-then-crash");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let broken = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .fail(&["pr", "create"], 1, "forge unavailable")
        .build();
    let output = run_with(&w, &agent, &broken, &["--no-ci-check"]);
    assert_ne!(output.status.code(), Some(0), "the review could not be opened");
    let pushed = git(&w.remote, &["rev-parse", "fix--COM-01"]);

    let second_agent = FakeAgent::builder().write_file("again.txt", "x\n").commit("feat: again").build();
    let second = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build();
    let output = run_with(&w, &second_agent, &second, &["--no-ci-check", "--resume"]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert_only_drafting(&second_agent, "the implement session is not started again");
    let creates = second.calls().iter().filter(|c| c.args.starts_with(&["pr".into(), "create".into()])).count();
    assert_eq!(creates, 1, "the review is opened exactly once: {:?}", second.calls());
    assert_eq!(git(&w.remote, &["rev-parse", "fix--COM-01"]), pushed, "the branch was not rewritten");
    assert!(stdout.contains(PR_URL), "{stdout}");
}

#[test]
fn a_run_stopped_by_its_gate_before_the_push_resumes_to_push_and_open_the_review_without_the_agent() {
    let w = world("gate-then-resume");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    let toml = w.repo.root().join(".orq/run.toml");
    std::fs::write(&toml, "gate = [\"false\"]\n").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);
    assert_ne!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(!git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "nothing was pushed");

    std::fs::write(&toml, "gate = [\"true\"]\n").unwrap();
    let second_agent = FakeAgent::builder().write_file("again.txt", "x\n").commit("feat: again").build();
    let provider = github();
    let output = run_with(&w, &second_agent, &provider, &["--no-ci-check", "--resume"]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert_only_drafting(&second_agent, "the implement session is not started again");
    assert!(git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "pushed on resume");
    let creates = provider.calls().iter().filter(|c| c.args.starts_with(&["pr".into(), "create".into()])).count();
    assert_eq!(creates, 1, "{:?}", provider.calls());
}

#[test]
fn an_agent_that_leaves_no_commit_blocks_the_run_and_nothing_is_pushed() {
    let w = world("nocommit");
    let agent = FakeAgent::builder().write_file("notes.txt", "wip\n").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(EXIT_BLOCKED), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: blocked"), "{stdout}");
    assert!(!stdout.contains("review:"), "{stdout}");
    assert!(!git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "nothing was pushed");
    assert_eq!(git(w.repo.root(), &["worktree", "list"]).lines().count(), 2, "the worktree is kept");
}

#[test]
fn a_red_ci_fails_the_run_but_still_reports_the_review_request() {
    let w = world("redci");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let red = "{\"total_count\":1,\"check_runs\":[{\"name\":\"test\",\"status\":\"completed\",\"conclusion\":\"failure\"}]}\n";
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .reply(&["api"], red)
        .build();

    let output = run_with(&w, &agent, &provider, &[]);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(EXIT_FAILED), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: failed"), "{stdout}");
    assert!(stdout.contains(&format!("review: {PR_URL}")), "{stdout}");
    assert!(stderr(&output).contains("test"), "the failing check is named: {}", stderr(&output));
    assert_eq!(git(w.repo.root(), &["worktree", "list"]).lines().count(), 2, "the worktree is kept");
}

#[test]
fn a_ci_failure_the_fix_session_calls_unrelated_blocks_the_run_and_says_what_to_fix_elsewhere() {
    let w = world("unrelated-ci");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let red = "{\"total_count\":1,\"check_runs\":[{\"name\":\"deploy\",\"status\":\"completed\",\"conclusion\":\"failure\"}]}\n";
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .reply(&["api"], red)
        .build();
    // Only a fix session is told where to report a failure that is not the run's.
    let bin = orq_test_support::process_temp_dir().join(format!("orq-run-bin-{}-unrelated-ci", std::process::id()));
    let _ = std::fs::remove_dir_all(&bin);
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nif [ -n \"$ORQ_UNRELATED_FAILURE_FILE\" ]; then echo 'deploy check is flaky' > \"$ORQ_UNRELATED_FAILURE_FILE\"; exit 0; fi\nexec '{}' \"$@\"\n",
            agent.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));

    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(EXIT_BLOCKED), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: blocked"), "{stdout}");
    assert!(stderr(&output).contains("unrelated") && stderr(&output).contains("deploy check is flaky"), "{}", stderr(&output));
    assert_eq!(git(w.repo.root(), &["worktree", "list"]).lines().count(), 2, "the worktree is kept");
}

#[test]
fn a_feature_with_a_run_in_progress_is_refused_with_its_own_exit_code() {
    let w = world("busy");
    let slow_bin = orq_test_support::process_temp_dir().join(format!("orq-run-slow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&slow_bin);
    std::fs::create_dir_all(&slow_bin).unwrap();
    let claude = slow_bin.join("claude");
    std::fs::write(&claude, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![slow_bin];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut first = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while !w.env.state.join("orq/runs").exists() && started.elapsed().as_secs() < 10 {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let second = run_with(&w, &agent, &github(), &["--no-ci-check"]);
    let _ = first.kill();
    let _ = first.wait();

    assert_eq!(second.status.code(), Some(EXIT_BUSY), "{}", stderr(&second));
    assert!(stderr(&second).contains("run-"), "the active run is named: {}", stderr(&second));
    assert!(second.stdout.is_empty());
}

#[test]
fn a_harness_found_through_a_relative_path_entry_still_runs_inside_the_worktree() {
    let w = world("relpath");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let absolute = path_with(&agent, &provider, &w.tag);
    let mut entries: Vec<PathBuf> = std::env::split_paths(&absolute).collect();
    // The harness directory is named relative to the directory orq is started from.
    let bin = entries[0].clone();
    entries[0] = PathBuf::from(bin.file_name().unwrap());

    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .current_dir(bin.parent().unwrap())
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(entries).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: review_ready"), "{stdout}");
}

#[test]
fn a_lock_that_cannot_be_released_fails_the_run_but_keeps_the_review_url() {
    let w = world("lockrelease");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let path = path_with(&agent, &provider, &w.tag);
    // The harness hands the feature's lock to another run before doing its work, so the
    // release at the end of this run is refused.
    let claude = std::env::split_paths(&path).next().unwrap().join("claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nfor f in '{}'/orq/locks/*/COM-01.lock; do echo run-other > \"$f\"; done\nexec '{}' \"$@\"\n",
            w.env.state.display(),
            agent.path().display()
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(EXIT_FAILED), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: failed"), "{stdout}");
    assert!(stdout.contains(&format!("review: {PR_URL}")), "the review is still named: {stdout}");
    assert!(stderr(&output).contains("lock"), "the release failure is reported: {}", stderr(&output));
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let started = std::time::Instant::now();
    while !done() {
        assert!(started.elapsed().as_secs() < 60, "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn a_detached_run_returns_at_once_while_the_run_goes_on_and_finishes_after_the_launcher_exited() {
    let w = world("detach");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    // The agent takes a moment, so the run is certainly still going when the launcher returns.
    let provider = github();
    let path = path_with(&agent, &provider, &w.tag);
    let claude = std::env::split_paths(&path).next().unwrap().join("claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nif [ \"$1\" = -p ]; then printf 'Add the feature\\n\\nThe feature.\\n'; exit 0; fi\nsleep 2\nexec '{}' \"$@\"\n",
            agent.path().display()
        ),
    )
    .unwrap();

    let started = std::time::Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check", "--detach"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert!(started.elapsed().as_secs() < 2, "the launcher did not wait for the run: {:?}", started.elapsed());
    let run_id = stdout.lines().find_map(|l| l.strip_prefix("run: ")).expect(&stdout).to_string();
    let log = stdout.lines().find_map(|l| l.strip_prefix("log: ")).expect(&stdout).to_string();
    assert!(stdout.contains("state: launched"), "{stdout}");

    // The run finishes on its own: its worktree is cleaned up and the review request exists.
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    wait_until("the detached run to complete", || {
        store.events(&run_id).map(|e| e.iter().any(|e| e.kind == "worktree.removed")).unwrap_or(false)
    });
    let events = store.events(&run_id).unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert!(kinds.contains(&"run.launched"), "{kinds:?}");
    assert!(kinds.contains(&"review.created"), "{kinds:?}");
    assert!(git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "the run branch was pushed");
    assert!(std::path::Path::new(&log).is_file(), "the run log exists at {log}");
}

#[test]
fn a_group_of_one_runs_the_feature_to_review_ready_like_feature_does_attached_or_detached() {
    let w = world("groupone");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_selecting(&w, &agent, &github(), &["--group", "COM-01"], &["--no-ci-check"]);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: review_ready"), "{stdout}");
    assert!(git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "the run branch was pushed");

    // Detached, the launcher forwards --group to the runner it starts and the run finishes.
    let w = world("groupone-detach");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let path = path_with(&agent, &provider, &w.tag);
    let claude = std::env::split_paths(&path).next().unwrap().join("claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nif [ \"$1\" = -p ]; then printf 'Add the feature\\n\\nThe feature.\\n'; exit 0; fi\nexec '{}' \"$@\"\n",
            agent.path().display()
        ),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--group", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check", "--detach"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    let run_id = stdout.lines().find_map(|l| l.strip_prefix("run: ")).expect(&stdout).to_string();
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    wait_until("the detached group run to complete", || {
        store.events(&run_id).map(|e| e.iter().any(|e| e.kind == "worktree.removed")).unwrap_or(false)
    });
    let kinds: Vec<String> = store.events(&run_id).unwrap().into_iter().map(|e| e.kind).collect();
    assert!(kinds.iter().any(|k| k == "review.created"), "{kinds:?}");
    assert!(git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "the run branch was pushed");
}

#[test]
fn a_group_of_several_features_runs_once_on_the_first_features_branch_with_one_review_request() {
    let w = world("groupmany");
    write_ledger(
        w.repo.root(),
        &[LedgerEntry::new("COM-01", "pending"), LedgerEntry::new("COM-02", "pending").depends_on(&["COM-01"])],
    );
    let agent = FakeAgent::builder()
        .write_file("one.txt", "1\n")
        .commit("feat: COM-01")
        .write_file("two.txt", "2\n")
        .commit("feat: COM-02")
        .build();
    let provider = github();

    // Given out of order: the dependency decides.
    let output = run_selecting(&w, &agent, &provider, &["--group", "COM-02,COM-01"], &["--no-ci-check"]);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: review_ready"), "{stdout}");
    let branches = git(&w.remote, &["branch", "--list"]);
    assert!(branches.contains("fix--COM-01"), "{branches}");
    assert!(!branches.contains("COM-02"), "one branch for the group: {branches}");
    let prompt = agent.first_run().expect("the agent ran").args.last().cloned().unwrap();
    assert!(prompt.starts_with("/implement-and-review COM-01 COM-02 "), "{prompt}");
    let creates: Vec<_> = provider.calls().into_iter().filter(|c| c.args.starts_with(&["pr".to_string(), "create".to_string()])).collect();
    assert_eq!(creates.len(), 1, "one review request");
    let title = creates[0].args.iter().skip_while(|a| *a != "--title").nth(1).cloned().expect("a title");
    assert!(title.contains("COM-01") && title.contains("COM-02"), "{title}");
}

#[test]
fn a_detached_run_that_may_not_start_is_refused_synchronously_and_launches_nothing() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "completed")]);
    let env = env("detach-refused");

    let output = run(repo.root(), &env, &["--feature", "COM-01", "--detach"]);

    assert_eq!(output.status.code(), Some(EXIT_INELIGIBLE), "{}", stderr(&output));
    assert!(state_is_empty(&env));
}

#[test]
fn a_detached_launch_of_a_feature_that_is_already_running_fails_with_the_reason_and_records_no_run() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let env = env("detach-busy");
    let inspected = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["project", "inspect", "--cwd"])
        .arg(repo.root())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();
    let project: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    let store = orq_orchestrator::RunStore::new(&env.state.join("orq"));
    store
        .start_run(&orq_orchestrator::RunRecord {
            run_id: "run-active".into(),
            project_id: project["id"].as_str().unwrap().to_string(),
            feature_id: "COM-01".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "orq/run-active".into(),
            worktree: PathBuf::from("/tmp/w"),
            repository: String::new(),
            profiles: Default::default(),
        })
        .unwrap();

    let output = run(repo.root(), &env, &["--feature", "COM-01", "--detach"]);

    assert_eq!(output.status.code(), Some(EXIT_BUSY), "{}", stderr(&output));
    assert!(stderr(&output).contains("already has an active run: run-active"), "{}", stderr(&output));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("state: launched"));
    assert_eq!(store.run_ids().unwrap(), ["run-active"], "no ghost run was recorded");
    assert!(!env.state.join("orq/logs").exists(), "no log directory was created");
}

#[test]
fn a_detached_group_is_refused_when_any_member_not_only_the_first_is_already_running() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending"), LedgerEntry::new("COM-02", "pending")]);
    let env = env("detach-group-busy");
    let inspected = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["project", "inspect", "--cwd"])
        .arg(repo.root())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();
    let project: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    let store = orq_orchestrator::RunStore::new(&env.state.join("orq"));
    store
        .start_run(&orq_orchestrator::RunRecord {
            run_id: "run-active".into(),
            project_id: project["id"].as_str().unwrap().to_string(),
            feature_id: "COM-02".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "orq/run-active".into(),
            worktree: PathBuf::from("/tmp/w"),
            repository: String::new(),
            profiles: Default::default(),
        })
        .unwrap();

    let output = run(repo.root(), &env, &["--group", "COM-01,COM-02", "--detach"]);

    assert_eq!(output.status.code(), Some(EXIT_BUSY), "{}", stderr(&output));
    assert!(stderr(&output).contains("COM-02") && stderr(&output).contains("run-active"), "{}", stderr(&output));
    assert_eq!(store.run_ids().unwrap(), ["run-active"], "no ghost run was recorded");
}

// Reads /proc, so it can only run where procfs exists.
#[cfg(target_os = "linux")]
#[test]
fn the_detached_runner_and_its_agent_live_in_a_session_of_their_own() {
    let w = world("detach-session");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let path = path_with(&agent, &provider, &w.tag);
    // The harness stand-in notes the session it runs in (field 6 of /proc/<pid>/stat).
    let note = w.env.state.join("agent-session");
    let claude = std::env::split_paths(&path).next().unwrap().join("claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nif [ \"$1\" = -p ]; then printf 'Add the feature\\n\\nThe feature.\\n'; exit 0; fi\nawk '{{print $6}}' /proc/$$/stat > '{}'\nexec '{}' \"$@\"\n",
            note.display(),
            agent.path().display()
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check", "--detach"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    let run_id = stdout.lines().find_map(|l| l.strip_prefix("run: ")).expect(&stdout).to_string();
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    wait_until("the detached run to complete", || {
        store.events(&run_id).map(|e| e.iter().any(|e| e.kind == "worktree.removed")).unwrap_or(false)
    });
    let events = store.events(&run_id).unwrap();
    let pid = events.iter().find(|e| e.kind == "run.launched").unwrap().payload["pid"].as_u64().unwrap();
    let agent_session: u64 = std::fs::read_to_string(&note).unwrap().trim().parse().unwrap();
    let own_session: u64 =
        std::fs::read_to_string("/proc/self/stat").unwrap().rsplit(')').next().unwrap().split_whitespace().nth(3).unwrap().parse().unwrap();
    assert_eq!(agent_session, pid, "the agent runs in the session the runner leads");
    assert_ne!(agent_session, own_session, "and not in the launcher's session");
}

#[test]
fn the_runner_log_and_its_directory_are_private_to_the_user() {
    use std::os::unix::fs::PermissionsExt;
    let w = world("detach-private");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let path = path_with(&agent, &provider, &w.tag);

    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check", "--detach"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    let run_id = stdout.lines().find_map(|l| l.strip_prefix("run: ")).expect(&stdout).to_string();
    let log = PathBuf::from(stdout.lines().find_map(|l| l.strip_prefix("log: ")).expect(&stdout));
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&log), 0o600, "the log is readable by its owner only");
    assert_eq!(mode(log.parent().unwrap()), 0o700, "the log directory is closed to others");
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    wait_until("the detached run to complete", || {
        store.events(&run_id).map(|e| e.iter().any(|e| e.kind == "worktree.removed")).unwrap_or(false)
    });
}

#[test]
fn detaching_keeps_an_option_value_that_is_spelled_like_the_flag() {
    let w = world("detach-value");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let path = path_with(&agent, &provider, &w.tag);
    // The repository is reached through a directory literally named `--detach`.
    let odd = orq_test_support::process_temp_dir().join(format!("orq-odd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&odd);
    std::fs::create_dir_all(&odd).unwrap();
    std::os::unix::fs::symlink(w.repo.root(), odd.join("--detach")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .current_dir(&odd)
        .args(["run", "--repo", "--detach"])
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check", "--detach"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    let run_id = stdout.lines().find_map(|l| l.strip_prefix("run: ")).expect(&stdout).to_string();
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    wait_until("the detached run to complete", || {
        store.events(&run_id).map(|e| e.iter().any(|e| e.kind == "worktree.removed")).unwrap_or(false)
    });
}

fn completed_events(w: &World, stdout: &str) -> Vec<(String, Option<String>)> {
    let run_id = stdout.lines().find_map(|l| l.strip_prefix("run: ")).expect("run id printed").trim().to_string();
    let World { env: scratch, .. } = w;
    let store = orq_orchestrator::RunStore::new(&scratch.state.join("orq"));
    store
        .events(&run_id)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "run.completed")
        .map(|e| {
            (
                e.payload["ending"].as_str().unwrap().to_string(),
                e.payload.get("reason").and_then(|r| r.as_str()).map(str::to_string),
            )
        })
        .collect()
}

#[test]
fn every_run_records_exactly_one_run_completed_event_with_how_it_ended() {
    let w = world("completed-happy");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert_eq!(completed_events(&w, &stdout), [("review_ready".to_string(), None)]);

    let w = world("completed-blocked");
    let agent = FakeAgent::builder().write_file("notes.txt", "wip\n").build();
    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(EXIT_BLOCKED), "{}\n{stdout}", stderr(&output));
    let done = completed_events(&w, &stdout);
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(done[0].0, "blocked");
    assert!(done[0].1.is_some(), "the reason is kept: {done:?}");
}

#[test]
fn a_run_whose_worktree_cannot_be_created_still_records_that_it_failed() {
    let w = world("completed-no-worktree");
    // A file where git keeps its worktree metadata makes `git worktree add` fail.
    std::fs::write(w.repo.root().join(".git").join("worktrees"), "in the way").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    assert_ne!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stderr(&output).contains("worktree"), "{}", stderr(&output));
    let World { env: scratch, .. } = &w;
    let runs = scratch.state.join("orq").join("runs");
    let ids: Vec<String> = std::fs::read_dir(&runs)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(ids.len(), 1, "{ids:?} {}", stderr(&output));
    let events = orq_orchestrator::RunStore::new(&scratch.state.join("orq")).events(&ids[0]).unwrap();
    let done: Vec<_> = events.iter().filter(|e| e.kind == "run.completed").collect();
    assert_eq!(done.len(), 1, "{events:?}");
    assert_eq!(done[0].payload["ending"].as_str(), Some("failed"));
    assert!(done[0].payload["reason"].as_str().is_some_and(|r| r.contains("worktree")), "{:?}", done[0].payload);
}

#[test]
fn a_configured_notification_command_hears_how_the_run_ended_and_its_failure_changes_nothing() {
    let w = world("notify");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let heard = w.repo.root().join("heard.txt");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    let toml = format!("notify_cmd = [\"sh\", \"-c\", \"printf %s \\\"$NOTIFY_MESSAGE\\\" > '{}'\"]\n", heard.display());
    std::fs::write(w.repo.root().join(".orq/run.toml"), toml).unwrap();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let message = std::fs::read_to_string(&heard).expect("the notifier ran");
    // The orchestrate script's words: `<repo>: <PR|MR> ready for review, <CI>. <url> (<branch>)`.
    let repo = w.repo.root().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(message, format!("{repo}: PR ready for review, CI not watched. {PR_URL} (fix--COM-01)"));

    let w = world("notifyfails");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), "notify_cmd = [\"no-such-notifier\"]\n").unwrap();
    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains("notification"), "{}", stderr(&output));
}

fn notify_world(tag: &str, mode: Option<&str>) -> (World, PathBuf) {
    let w = world(tag);
    let heard = w.repo.root().join("heard.txt");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    let mut toml = format!("notify_cmd = [\"sh\", \"-c\", \"printf '%s\\\\n' \\\"$NOTIFY_MESSAGE\\\" >> '{}'\"]\n", heard.display());
    if let Some(mode) = mode {
        toml.push_str(&format!("notify_on = \"{mode}\"\n"));
    }
    std::fs::write(w.repo.root().join(".orq/run.toml"), toml).unwrap();
    (w, heard)
}

#[test]
fn a_run_that_cannot_create_its_worktree_is_still_announced() {
    let (w, heard) = notify_world("notifywt", None);
    // A file where the worktrees' parent directory must go.
    std::fs::write(w.repo.root().join(".claude"), "in the way").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    assert!(!output.status.success(), "{}", stderr(&output));
    let heard = std::fs::read_to_string(&heard).expect("the notifier ran");
    assert!(heard.starts_with("orq: COM-01 failed"), "{heard}");
}

#[test]
fn notify_on_failures_stays_quiet_for_a_clean_run_and_speaks_for_one_that_stops() {
    let (w, heard) = notify_world("notifyquiet", Some("failures"));
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!heard.exists(), "a clean run is not announced under notify_on = failures");

    // An agent that changes nothing leaves no work to review: the run stops, and that is announced.
    let (w, heard) = notify_world("notifystops", Some("failures"));
    let idle = FakeAgent::builder().build();
    let _ = run_with(&w, &idle, &github(), &["--no-ci-check"]);
    let heard = std::fs::read_to_string(&heard).expect("a run that stops is announced");
    assert!(heard.starts_with("orq: COM-01 blocked") || heard.starts_with("orq: COM-01 failed"), "{heard}");
}

#[test]
fn publish_pushes_the_checked_out_branch_and_opens_the_review_without_a_worktree_or_an_implement_session() {
    let w = world("publish");
    git(w.repo.root(), &["checkout", "--quiet", "-b", "topic"]);
    std::fs::write(w.repo.root().join("topic.txt"), "work\n").unwrap();
    git(w.repo.root(), &["add", "topic.txt"]);
    git(w.repo.root(), &["-c", "user.name=Topic", "-c", "user.email=topic@example.invalid", "commit", "--quiet", "-m", "feat: the topic"]);
    // The agent is only the description drafter here: it must not be asked to implement anything.
    let agent = FakeAgent::builder().build();
    let provider = github();

    let output = run_with(&w, &agent, &provider, &["--publish", "--no-ci-check"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains(&format!("review: {PR_URL}")), "{}", String::from_utf8_lossy(&output.stdout));
    let pushed = Command::new("git").arg("-C").arg(&w.remote).args(["rev-parse", "--verify", "refs/heads/topic"]).output().unwrap();
    assert!(pushed.status.success(), "the branch was pushed");
    assert!(provider.calls().iter().any(|c| c.args.starts_with(&["pr".to_string(), "create".to_string()])));
    assert!(!w.repo.root().join(".claude/worktrees").exists(), "no worktree was created");
    let head = Command::new("git").arg("-C").arg(w.repo.root()).args(["symbolic-ref", "--short", "HEAD"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "topic");
    // The only agent session is the description drafter, run in the repository itself.
    let first = agent.first_run().expect("the drafter ran");
    assert_eq!(first.args, agent.last_run().unwrap().args, "exactly one agent session");
}

#[test]
fn publish_refuses_a_checkout_it_cannot_publish_and_leaves_everything_alone() {
    // (tag, how to prepare the repo, what the refusal must say)
    let cases: [(&str, fn(&Path), &str); 4] = [
        ("pubtarget", |_| {}, "is the target branch"),
        (
            "pubdetached",
            |r| {
                git(r, &["checkout", "--quiet", "--detach"]);
            },
            "detached"),
        (
            "pubdirty",
            |r| {
                git(r, &["checkout", "--quiet", "-b", "topic"]);
                std::fs::write(r.join("topic.txt"), "work\n").unwrap();
                git(r, &["add", "topic.txt"]);
                git(r, &["-c", "user.name=Topic", "-c", "user.email=topic@example.invalid", "commit", "--quiet", "-m", "topic"]);
                std::fs::write(r.join("topic.txt"), "changed\n").unwrap();
            },
            "uncommitted changes",
        ),
        (
            "pubempty",
            |r| {
                git(r, &["checkout", "--quiet", "-b", "topic"]);
            },
            "no commits beyond"),
    ];
    for (tag, prepare, message) in cases {
        let w = world(tag);
        prepare(w.repo.root());
        let agent = FakeAgent::builder().build();
        let provider = github();
        let output = run_with(&w, &agent, &provider, &["--publish", "--no-ci-check"]);
        assert!(!output.status.success(), "{tag}: {}", String::from_utf8_lossy(&output.stdout));
        assert!(stderr(&output).contains(message), "{tag}: {}", stderr(&output));
        assert!(provider.calls().is_empty(), "{tag}: no review was asked for");
        assert!(agent.last_run().is_none(), "{tag}: no agent session ran");
        assert!(!w.repo.root().join(".claude/worktrees").exists(), "{tag}: no worktree");
    }
}

#[test]
fn the_implement_session_is_reachable_from_the_app_and_a_failed_run_starts_an_explainer_session() {
    let w = world("explainer");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").exit_code(3).build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    assert_ne!(output.status.code(), Some(0), "{}", stderr(&output));
    let first = agent.first_run().expect("the implement session ran").args;
    let at = first.iter().position(|a| a == "--remote-control").expect("remote control is on by default");
    assert!(first[at + 1].starts_with("orq/") || !first[at + 1].is_empty(), "{first:?}");
    wait_until("the explainer session", || agent.last_run().is_some_and(|run| run.args != first));
    let explain = agent.last_run().unwrap().args;
    assert!(explain.iter().any(|a| a == "--remote-control"), "{explain:?}");
    assert!(explain.last().unwrap().contains("the implement session failed"), "{explain:?}");
}

#[test]
fn both_switches_off_in_the_settings_file_start_no_remote_control_and_no_explainer() {
    let w = world("noexplainer");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), "remote_control = false\nexplain_failures = false\n").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").exit_code(3).build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    assert_ne!(output.status.code(), Some(0), "{}", stderr(&output));
    let first = agent.first_run().expect("the implement session ran").args;
    assert!(!first.iter().any(|a| a == "--remote-control"), "{first:?}");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(agent.last_run().unwrap().args, first, "no second session was started");
}

#[test]
fn remote_control_off_with_the_explainer_on_starts_an_explainer_that_is_not_reachable_from_the_app() {
    let w = world("explainer-local");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), "remote_control = false\n").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").exit_code(3).build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    assert_ne!(output.status.code(), Some(0), "{}", stderr(&output));
    let first = agent.first_run().expect("the implement session ran").args;
    wait_until("the explainer session", || agent.last_run().is_some_and(|run| run.args != first));
    let explain = agent.last_run().unwrap().args;
    assert!(!explain.iter().any(|a| a == "--remote-control"), "{explain:?}");
}

/// As `run_with`, but the push is left to the project's settings, and a `claude` writes down the push
/// session it is asked for at the returned path instead of running the fake agent for it.
fn run_with_push_allowed(w: &World, agent: &FakeAgent, provider: &FakeProvider) -> (Output, PathBuf) {
    let bin = orq_test_support::process_temp_dir().join(format!("orq-run-push-{}-{}", std::process::id(), w.tag));
    let _ = std::fs::remove_dir_all(&bin);
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in *'--tools PushNotification'*) {{ [ -t 0 ] && echo terminal; echo \"cwd=$PWD\"; echo \"presence=$CLAUDE_CODE_DISABLE_NOTIFICATION_PRESENCE_CHECK\"; echo \"memory=$CLAUDE_CODE_DISABLE_CLAUDE_MDS\"; printf '%s\\n' \"$@\"; }} > \"$0.tmp\"; mv \"$0.tmp\" \"$0.heard\"; exit 0;; esac\nexec '{}' \"$@\"\n",
        agent.path().display()
    );
    std::fs::write(&claude, script).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin.clone()];
    dirs.extend(std::env::split_paths(&path_with(agent, provider, &w.tag)));
    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("ORQ_CLAUDE_PUSH")
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();
    (output, bin.join("claude.heard"))
}

#[test]
fn claude_push_is_on_by_default_and_reaches_the_app_through_a_connected_terminal_session() {
    let w = world("claudepush");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let (output, heard) = run_with_push_allowed(&w, &agent, &github());

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    wait_until("the push session", || heard.exists());
    let heard = std::fs::read_to_string(&heard).unwrap();
    let lines: Vec<&str> = heard.lines().collect();
    assert_eq!(lines[0], "terminal", "a print session can never deliver: {heard}");
    let cwd = lines[1].strip_prefix("cwd=").unwrap();
    assert_eq!(Path::new(cwd).canonicalize().unwrap(), w.repo.root().canonicalize().unwrap(), "a new directory would stop at the trust prompt: {heard}");
    assert_eq!(lines[2], "presence=1", "{heard}");
    assert_eq!(lines[3], "memory=1", "{heard}");
    assert!(!lines.contains(&"-p"), "{heard}");
    assert!(lines.iter().any(|l| l.starts_with("--remote-control=")), "{heard}");
    assert!(heard.contains("ready for review") && heard.contains(PR_URL), "{heard}");
}

#[test]
fn claude_push_stays_off_when_turned_off_or_remote_control_is_off_and_follows_notify_on() {
    for (tag, settings) in [
        ("claudepushoff", "claude_push = false\n"),
        ("claudepushlocal", "remote_control = false\nexplain_failures = false\n"),
        ("claudepushquiet", "notify_on = \"failures\"\n"),
    ] {
        let w = world(tag);
        std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
        std::fs::write(w.repo.root().join(".orq/run.toml"), settings).unwrap();
        let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
        let (output, heard) = run_with_push_allowed(&w, &agent, &github());
        assert_eq!(output.status.code(), Some(0), "{tag}: {}", stderr(&output));
        std::thread::sleep(std::time::Duration::from_millis(800));
        assert!(!heard.exists(), "{tag}");
    }
}

#[test]
fn a_group_run_is_announced_once_for_its_one_branch_and_review() {
    let (w, heard) = notify_world("notifygroup", None);
    write_ledger(
        w.repo.root(),
        &[LedgerEntry::new("COM-01", "pending"), LedgerEntry::new("COM-02", "pending").depends_on(&["COM-01"])],
    );
    let agent = FakeAgent::builder().write_file("one.txt", "1\n").commit("feat: COM-01").write_file("two.txt", "2\n").commit("feat: COM-02").build();

    let output = run_selecting(&w, &agent, &github(), &["--group", "COM-02,COM-01"], &["--no-ci-check"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let heard = std::fs::read_to_string(&heard).expect("the notifier ran");
    let lines: Vec<_> = heard.lines().collect();
    assert_eq!(lines.len(), 1, "{heard}");
    assert!(lines[0].contains("ready for review, CI not watched. https://"), "{heard}");
    assert!(lines[0].ends_with("(fix--COM-01)"), "the group's one branch: {heard}");
}

/// Runs `orq run push-send` with a `claude` that is the given shell script, in a directory of its own.
fn push_send_with(tag: &str, claude_script: &str) -> Output {
    let dir = std::env::temp_dir().join(format!("orq-push-send-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let claude = dir.join("claude");
    std::fs::write(&claude, format!("#!/bin/sh\n{claude_script}\n")).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "push-send", "--repo"])
        .arg(&dir)
        .env("ORQ_PUSH_TEXT", "orq: COM-01 review_ready")
        .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
        .output()
        .unwrap()
}

#[test]
fn a_push_session_that_ends_without_the_tool_having_run_is_a_failure() {
    let silent = push_send_with("silent", "exit 0");
    let failing = push_send_with("failing", "exit 3");

    for output in [silent, failing] {
        assert_ne!(output.status.code(), Some(0), "nothing was sent, so nothing may say it was");
        assert!(stderr(&output).contains("push"), "{}", stderr(&output));
    }
}

#[test]
fn a_push_session_whose_tool_ran_is_a_delivery() {
    let sent = push_send_with("sent", "for a in \"$@\"; do m=$(printf '%s' \"$a\" | sed -n \"s/.*touch '\\([^']*\\)'.*/\\1/p\"); [ -n \"$m\" ] && touch \"$m\"; done; exit 0");

    assert_eq!(sent.status.code(), Some(0), "{}", stderr(&sent));
}

#[test]
fn a_gate_failure_the_fix_session_calls_unrelated_blocks_the_run_and_says_what_to_fix_elsewhere() {
    let w = world("unrelated-gate");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), "gate = [\"false\"]\n").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    // Only a fix session is told where to report a failure that is not the run's.
    let bin = orq_test_support::process_temp_dir().join(format!("orq-run-bin-{}-unrelated", std::process::id()));
    let _ = std::fs::remove_dir_all(&bin);
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nif [ -n \"$ORQ_UNRELATED_FAILURE_FILE\" ]; then echo 'billing clock test is flaky' > \"$ORQ_UNRELATED_FAILURE_FILE\"; exit 0; fi\nexec '{}' \"$@\"\n",
            agent.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));

    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(EXIT_BLOCKED), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: blocked"), "{stdout}");
    assert!(stderr(&output).contains("unrelated") && stderr(&output).contains("billing clock test is flaky"), "{}", stderr(&output));
    assert!(!git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "nothing was pushed");
    assert_eq!(git(w.repo.root(), &["worktree", "list"]).lines().count(), 2, "the worktree is kept");
}
