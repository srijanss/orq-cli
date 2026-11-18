//! A run the queue starts is unattended: nobody watches it, so its session runs headless and is told
//! never to ask, and the run never waits for an answer. What it cannot decide either becomes a draft
//! follow-up or ends the run blocked with the reason.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

struct World {
    repo: GitFixture,
    _remote: PathBuf,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    bin: PathBuf,
    prompts: PathBuf,
    argv: PathBuf,
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

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A repository with a pushed `main` and a pending COM-01, and a `claude` that records its arguments,
/// runs `agent`, then runs `then` in the worktree (a shell snippet) and writes `marker_text` into its
/// finish marker.
fn world(tag: &str, agent: &FakeAgent, then: &str, marker_text: &str) -> World {
    let root = orq_test_support::process_temp_dir().join(format!("orq-unattended-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config/orq")).unwrap();
    std::fs::create_dir_all(root.join("state")).unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    std::fs::write(repo.root().join(".orq/run.toml"), "explain_failures = false\n").unwrap();
    // The review ledgers a session leaves are ignored, as a project ignores them.
    std::fs::create_dir_all(repo.root().join(".git/info")).unwrap();
    std::fs::write(repo.root().join(".git/info/exclude"), ".tdd-review-rounds.json\n.tdd-review-findings.json\n").unwrap();
    let ssh = root.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let claude = root.join("bin/claude");
    std::fs::write(
        &claude,
        format!(
            concat!(
                "#!/bin/sh\n",
                "printf '%s\\n' \"$@\" >> '{argv}'\n",
                "for a; do last=$a; done\n",
                "printf '%s\\n--\\n' \"$last\" >> '{prompts}'\n",
                "sed -i 's/\"pending\"/\"in_progress\"/' '{features}'\n",
                "'{agent}' \"$@\" || exit $?\n",
                "for a; do last=$a; done\n",
                "marker=$(printf '%s' \"$last\" | grep -o '/[^ `]*implement\\.done' | head -n 1)\n",
                "{then}\n",
                "printf '{marker_text}' > \"$marker\"\n",
            ),
            argv = root.join("argv.log").display(),
            features = repo.root().join(".tdd-features.json").display(),
            prompts = root.join("prompts.log").display(),
            agent = agent.path().display(),
            then = then,
            marker_text = marker_text,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    World {
        repo,
        _remote: remote,
        state: root.join("state"),
        config: root.join("config"),
        ssh,
        bin: root.join("bin"),
        prompts: root.join("prompts.log"),
        argv: root.join("argv.log"),
    }
}

fn run(w: &World, extra: &[&str]) -> Output {
    let mut args = vec!["run", "--repo", w.repo.root().to_str().unwrap(), "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"];
    args.extend_from_slice(extra);
    orq(w, &args)
}

fn orq(w: &World, args: &[&str]) -> Output {
    let provider = FakeProvider::builder().reply(&["pr", "list"], "[]\n").build();
    orq_command(w, &provider.bin_dir()).args(args).output().unwrap()
}

/// `orq` in the world's sandbox, with its fake agent and the provider in `provider_bin` first on the path.
fn orq_command(w: &World, provider_bin: &Path) -> Command {
    let mut dirs = vec![w.bin.clone(), provider_bin.to_path_buf()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut command = orq_test_support::sandboxed_command(env!("CARGO_BIN_EXE_orq"));
    command
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("TDD_FEATURES_PATH");
    command
}

fn first_prompt(w: &World, output: &Output) -> String {
    let prompts = std::fs::read_to_string(&w.prompts).unwrap_or_else(|_| panic!("no session ran\n{}\n{}", text(&output.stdout), text(&output.stderr)));
    prompts.split("\n--\n").next().unwrap().to_string()
}

fn argv_lines(w: &World) -> Vec<String> {
    std::fs::read_to_string(&w.argv).unwrap().lines().map(str::to_string).collect()
}

fn feature_agent() -> FakeAgent {
    FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build()
}

#[test]
fn an_unattended_run_s_session_runs_headless_and_is_told_nobody_is_watching() {
    let agent = feature_agent();
    let w = world("wording", &agent, "", "OK");

    let output = run(&w, &["--unattended"]);

    let prompt = first_prompt(&w, &output);
    let argv = argv_lines(&w);
    assert_eq!(argv.first().map(String::as_str), Some("-p"), "{argv:?}");
    assert!(!argv.iter().any(|a| a == "--remote-control"), "nobody reaches an unattended session from the app: {argv:?}");
    assert!(prompt.contains("Nobody is watching this session"), "{prompt}");
    assert!(!prompt.contains("A person is watching this session"), "{prompt}");
    assert!(!prompt.contains("AskUserQuestion"), "{prompt}");
}

/// A shell snippet that leaves a review ledger whose one review escalated at round 3 with `findings`
/// open, each `(id, severity, category)`, as a session does whose review loop hit its round cap.
fn escalated_review(findings: &[(&str, &str, &str)]) -> String {
    let rounds: Vec<String> = findings
        .iter()
        .enumerate()
        .map(|(n, (id, severity, category))| {
            format!(
                "{{\"id\":\"{id}\",\"severity\":\"{severity}\",\"category\":\"{category}\",\"status\":\"open\",\"file\":\"src/lib.rs\",\"line\":{line},\"claim\":\"the {id} claim\",\"suggestion\":\"fix {id}\"}}",
                line = n + 1
            )
        })
        .collect();
    let live: Vec<String> = findings.iter().map(|(id, severity, _)| format!("{{\"id\":\"{id}\",\"severity\":\"{severity}\",\"status\":\"open\"}}")).collect();
    format!(
        concat!(
            "base=$(git rev-parse HEAD~1); head=$(git rev-parse HEAD)\n",
            "printf '{{\"review:%s\":[{{\"start\":\"%s\",\"head\":\"%s\",\"reviewer\":\"pi\",\"model\":\"m\",\"round\":3,",
            "\"recordedAt\":\"2026-11-07T12:00:00+00:00\",\"findings\":[{rounds}]}}]}}' \"$base\" \"$base\" \"$head\" > .tdd-review-rounds.json\n",
            "printf '{{\"review:%s\":[{live}]}}' \"$base\" > .tdd-review-findings.json",
        ),
        rounds = rounds.join(","),
        live = live.join(","),
    )
}

fn ledger(repo: &Path) -> Vec<serde_json::Value> {
    serde_json::from_slice::<serde_json::Value>(&std::fs::read(repo.join(".tdd-features.json")).unwrap()).unwrap().as_array().unwrap().clone()
}

#[test]
fn an_unattended_run_whose_review_escalated_drafts_a_follow_up_per_open_finding_and_merges() {
    let agent = feature_agent();
    let w = world("drafts", &agent, &escalated_review(&[("off-by-one", "medium", "bugs"), ("no-edge-test", "medium", "missing-tests")]), "PROBLEM\\nthe review escalated after 3 rounds\\n");

    let output = run(&w, &["--unattended", "--delivery", "merge-local"]);

    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert!(stdout.contains("state: merged"), "{stdout}\n{stderr}");
    let drafted = stderr.lines().find_map(|l| l.split("Follow-ups drafted: ").nth(1)).unwrap_or_else(|| panic!("{stdout}\n{stderr}"));
    let drafted: Vec<&str> = drafted.split(", ").collect();
    assert!(matches!(drafted[..], [a, b] if named(a, "review-followup-COM-01-off-by-one") && named(b, "review-followup-COM-01-no-edge-test")), "{stderr}");
    let ledger = ledger(w.repo.root());
    let draft = ledger.iter().find(|e| named(e["featureName"].as_str().unwrap_or_default(), "review-followup-COM-01-off-by-one")).unwrap_or_else(|| panic!("{ledger:?}"));
    assert_eq!(draft["status"], "draft");
    assert_eq!(draft["dependsOn"], serde_json::json!([]));
    let description = draft["description"].as_str().unwrap();
    for part in ["off-by-one", "src/lib.rs:1", "bugs", "medium", "the off-by-one claim", "fix off-by-one"] {
        assert!(description.contains(part), "{part} missing from {description}");
    }
    assert!(ledger.iter().any(|e| named(e["featureName"].as_str().unwrap_or_default(), "review-followup-COM-01-no-edge-test") && e["status"] == "draft"), "{ledger:?}");
    // orq drafted them, not the session: they are not work the session added beside its own.
    assert!(!stderr.contains("also added"), "{stderr}");
}

/// Whether `name` is the follow-up name `readable` with the hash every follow-up name ends in.
fn named(name: &str, readable: &str) -> bool {
    name.strip_prefix(readable).and_then(|rest| rest.strip_prefix('-')).is_some_and(|hash| hash.len() == 12 && hash.chars().all(|c| c.is_ascii_hexdigit()))
}

fn followups(repo: &Path) -> Vec<serde_json::Value> {
    ledger(repo).into_iter().filter(|e| e["featureName"].as_str().is_some_and(|n| n.starts_with("review-followup-"))).collect()
}

#[test]
fn an_unattended_run_whose_review_escalated_with_a_high_or_security_finding_ends_blocked_naming_it() {
    for (tag, serious) in [("high", ("leak-to-log", "high", "bugs")), ("security", ("token-in-url", "medium", "security"))] {
        let agent = feature_agent();
        let w = world(tag, &agent, &escalated_review(&[serious, ("minor-thing", "medium", "bugs")]), "PROBLEM\\nthe review escalated after 3 rounds\\n");

        let output = run(&w, &["--unattended"]);

        let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
        assert!(stdout.contains("state: blocked"), "{tag}: {stdout}\n{stderr}");
        assert!(stderr.contains(serious.0), "{tag}: {stderr}");
        assert!(!stderr.contains("Follow-ups drafted"), "{tag}: {stderr}");
        assert_eq!(followups(w.repo.root()), Vec::<serde_json::Value>::new(), "{tag}");
    }
}

#[test]
fn an_unattended_run_whose_session_needs_a_decision_ends_blocked_with_it() {
    let agent = feature_agent();
    let w = world("decision", &agent, "", "PROBLEM\\nneeds a decision: whether the cache may be shared between projects\\n");

    let output = run(&w, &["--unattended"]);

    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert!(stdout.contains("state: blocked"), "{stdout}\n{stderr}");
    assert!(stderr.contains("needs a decision: whether the cache may be shared between projects"), "{stderr}");
}

/// A shell snippet that fires the worktree's needs-input hook as Claude does when the session is about
/// to ask `question`.
fn ask(question: &str) -> String {
    format!(
        concat!(
            "hook=$(jq -r '.hooks.PreToolUse[]?.hooks[]?.command' .claude/settings.local.json | grep ' run needs-input ' | head -n 1)\n",
            "test -n \"$hook\" || {{ echo 'no needs-input hook' >&2; exit 3; }}\n",
            "printf '%s' '{{\"hook_event_name\":\"PreToolUse\",\"tool_name\":\"AskUserQuestion\",\"tool_input\":{{\"questions\":[{{\"question\":\"{question}\"}}]}}}}' | sh -c \"$hook\"",
        ),
        question = question
    )
}

#[test]
fn an_unattended_run_whose_session_asks_a_question_ends_blocked_with_it() {
    let agent = feature_agent();
    let w = world("asks", &agent, &ask("Which cache should I use?"), "OK");

    let output = run(&w, &["--unattended"]);

    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert!(stdout.contains("state: blocked"), "{stdout}\n{stderr}");
    assert!(stderr.contains("asked a question in an unattended run: Which cache should I use?"), "{stderr}");
}

#[test]
fn an_unattended_run_is_listed_as_unattended_and_a_run_started_by_hand_stays_attended() {
    let agent = feature_agent();
    let w = world("listed", &agent, "", "OK");
    let unattended = run(&w, &["--unattended"]);
    let run_id = text(&unattended.stdout).lines().find_map(|l| l.strip_prefix("run: ").map(str::to_string)).expect("a run id");

    let listed = orq(&w, &["runs", "--repo", w.repo.root().to_str().unwrap()]);
    let listed = text(&listed.stdout);
    let row = listed.split(&format!("{run_id}\t")).nth(1).unwrap_or_else(|| panic!("{listed}"));
    assert!(row.lines().nth(1).is_some_and(|l| l.trim() == "Unattended (queue)"), "{listed}");
    let json = orq(&w, &["runs", "--repo", w.repo.root().to_str().unwrap(), "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(json["runs"][0]["unattended"], true, "{json}");

    let agent = feature_agent();
    let w = world("by-hand", &agent, "", "OK");
    run(&w, &[]);
    assert!(argv_lines(&w).first().is_some_and(|a| a != "-p"), "a run started by hand is attended: {:?}", argv_lines(&w));
    let listed = orq(&w, &["runs", "--repo", w.repo.root().to_str().unwrap()]);
    assert!(!text(&listed.stdout).contains("Unattended"), "{}", text(&listed.stdout));
    let json = orq(&w, &["runs", "--repo", w.repo.root().to_str().unwrap(), "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(json["runs"][0]["unattended"], false, "{json}");
}

/// An `orq serve` child, killed and reaped when dropped.
struct Served(std::process::Child);

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_run_the_queue_starts_runs_unattended_outside_herdr_even_while_herdr_runs() {
    let agent = feature_agent();
    let w = world("queued", &agent, "", "OK");
    let provider = FakeProvider::builder().reply(&["pr", "list"], "[]\n").build();
    // Herdr looks to be running, and any call to it is recorded.
    let herdr_calls = w.bin.join("herdr.log");
    std::fs::write(w.bin.join("herdr"), format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 1\n", herdr_calls.display())).unwrap();
    std::fs::set_permissions(w.bin.join("herdr"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let repo = w.repo.root().to_str().unwrap();
    let queued = orq_command(&w, &provider.bin_dir()).args(["queue", "add", "COM-01", "--repo", repo]).output().unwrap();
    assert!(queued.status.success(), "{}", text(&queued.stderr));

    let mut serve = orq_command(&w, &provider.bin_dir());
    serve
        .args(["serve", "--listen", "127.0.0.1:0"])
        .current_dir(repo)
        .env("ORQ_REMOTE_PROJECTS_ROOT", w.repo.root().parent().unwrap())
        .env("HERDR_SOCKET_PATH", w.bin.join("herdr.sock"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let _served = Served(serve.spawn().unwrap());

    // The queue starts COM-01 on its first round; the run ends once its fake session has.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    let run = loop {
        let listed = orq(&w, &["runs", "--repo", repo, "--json"]);
        let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap_or_default();
        let ended = listed["runs"].as_array().and_then(|runs| runs.iter().find(|r| !matches!(r["phase"].as_str(), Some("running") | None)).cloned());
        if let Some(run) = ended {
            break run;
        }
        assert!(std::time::Instant::now() < deadline, "the queued run did not end in time: {listed}");
        std::thread::sleep(std::time::Duration::from_millis(250));
    };

    assert_eq!(run["unattended"], true, "{run}");
    assert_eq!(argv_lines(&w).first().map(String::as_str), Some("-p"), "{:?}", argv_lines(&w));
    assert!(!herdr_calls.exists(), "the queue's run went to Herdr: {}", std::fs::read_to_string(&herdr_calls).unwrap_or_default());
}

/// A decision the session says it cannot make is a person's, even when its review escalated too with
/// findings that would only be drafted: the run ends blocked on the decision, and nothing is drafted.
#[test]
fn an_unattended_run_that_needs_a_decision_ends_blocked_on_it_even_when_its_review_escalated() {
    let agent = feature_agent();
    let w = world("decision-and-escalation", &agent, &escalated_review(&[("minor-thing", "medium", "bugs")]), "PROBLEM\\nneeds a decision: whether to drop the v1 API\\n");

    let output = run(&w, &["--unattended", "--delivery", "merge-local"]);

    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert!(stdout.contains("state: blocked"), "{stdout}\n{stderr}");
    assert!(stderr.contains("needs a decision: whether to drop the v1 API"), "{stderr}");
    assert_eq!(followups(w.repo.root()), Vec::<serde_json::Value>::new());
}

/// What an unattended run does with an escalated review does not hang on how its session reported: one
/// that wrote OK after its review escalated is still blocked by a security finding, and its other open
/// findings are still drafted.
#[test]
fn an_unattended_run_whose_session_reported_ok_after_its_review_escalated_is_judged_by_the_review() {
    let agent = feature_agent();
    let w = world("ok-security", &agent, &escalated_review(&[("token-in-url", "medium", "security")]), "OK");
    let output = run(&w, &["--unattended", "--delivery", "merge-local"]);
    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert!(stdout.contains("state: blocked"), "{stdout}\n{stderr}");
    assert!(stderr.contains("token-in-url"), "{stderr}");

    let agent = feature_agent();
    let w = world("ok-drafts", &agent, &escalated_review(&[("off-by-one", "medium", "bugs")]), "OK");
    let output = run(&w, &["--unattended", "--delivery", "merge-local"]);
    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert!(stdout.contains("state: merged"), "{stdout}\n{stderr}");
    assert!(stderr.lines().find_map(|l| l.split("Follow-ups drafted: ").nth(1)).is_some_and(|n| named(n, "review-followup-COM-01-off-by-one")), "{stderr}");
}

/// Finding ids that differ only in characters a ledger name cannot hold still get a follow-up each,
/// under names of their own.
#[test]
fn findings_whose_ids_clean_to_the_same_name_each_get_a_follow_up_of_their_own() {
    let agent = feature_agent();
    let w = world("collide", &agent, &escalated_review(&[("a/b", "medium", "bugs"), ("a:b", "medium", "bugs"), ("a-b", "medium", "bugs")]), "PROBLEM\\nthe review escalated after 3 rounds\\n");

    let output = run(&w, &["--unattended", "--delivery", "merge-local"]);

    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert!(stdout.contains("state: merged"), "{stdout}\n{stderr}");
    let names: std::collections::BTreeSet<String> = followups(w.repo.root()).iter().map(|e| e["featureName"].as_str().unwrap().to_string()).collect();
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(names.iter().all(|n| named(n, "review-followup-COM-01-a-b")), "{names:?}");
}

/// A session that crashed, leaving no finish report, has failed whatever review it left: an escalated
/// one does not carry the run on, and nothing is drafted from it.
#[test]
fn an_unattended_run_whose_session_crashed_after_its_review_escalated_fails_and_drafts_nothing() {
    let agent = feature_agent();
    let crash = format!("{}
exit 7", escalated_review(&[("minor-thing", "medium", "bugs")]));
    let w = world("crashed", &agent, &crash, "");

    let output = run(&w, &["--unattended", "--delivery", "merge-local"]);

    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert!(stdout.contains("state: failed"), "{stdout}\n{stderr}");
    assert!(!stdout.contains("state: merged"), "{stdout}");
    assert_eq!(followups(w.repo.root()), Vec::<serde_json::Value>::new());
}
