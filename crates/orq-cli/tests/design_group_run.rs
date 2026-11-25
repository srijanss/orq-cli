//! A group run whose frontend members share one design package is a design group: each member gets its
//! own implement session, the gate and, for a frontend member, its own design loop, in the group's order,
//! and the group is delivered once. A group mixing packages is refused at start.

use std::path::Path;
use std::process::{Command, Output};

use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The orq binary with this test's own orq folders, kept beside its repository.
fn orq(repo: &GitFixture) -> Command {
    let home = repo.root().with_extension("orq-home");
    let mut command = orq_test_support::sandboxed_command(env!("CARGO_BIN_EXE_orq"));
    command
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("TDD_FEATURES_PATH");
    command
}

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap()
        .success());
}

fn check_group(repo: &GitFixture, group: &str) -> Output {
    orq(repo)
        .args(["run", "--repo"])
        .arg(repo.root())
        .args([
            "--remote", "origin", "--target", "main", "--group", group, "--check",
        ])
        .output()
        .unwrap()
}

fn mark_frontend(repo: &GitFixture, feature: &str) {
    let kind = orq(repo)
        .args(["feature", "kind", "--repo"])
        .arg(repo.root())
        .args(["--id", feature, "frontend"])
        .output()
        .unwrap();
    assert!(kind.status.success(), "{}", text(&kind.stderr));
}

/// A run of the store, for the group runner's events.
fn store(tag: &str) -> (orq_orchestrator::RunStore, std::path::PathBuf) {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-design-group-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let store = orq_orchestrator::RunStore::new(&root.join("state"));
    store
        .create(&orq_orchestrator::RunRecord {
            delivery: Default::default(),
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "hero".to_string(),
            remote: "origin".to_string(),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: "0".repeat(40),
            branch: "fix--hero".to_string(),
            worktree: root.join("worktree"),
            repository: String::new(),
            profiles: Default::default(),
        })
        .unwrap();
    (store, root)
}

fn member(feature: &str, components: &[&str]) -> orq_orchestrator::DesignGroupMember {
    orq_orchestrator::DesignGroupMember {
        feature: feature.to_string(),
        frontend: !components.is_empty(),
        components: components.iter().map(|c| c.to_string()).collect(),
    }
}

/// Fake steps: every call is written down; `stop` names the step (`"design b"`) that stops the group.
#[derive(Default)]
struct Steps {
    calls: Vec<String>,
    stop: Option<String>,
}

impl Steps {
    fn step(&mut self, call: String) -> Result<Option<String>, orq_orchestrator::StoreError> {
        let stops = self.stop.as_deref() == Some(call.as_str());
        self.calls.push(call.clone());
        Ok(stops.then(|| format!("{call} needs review")))
    }
}

impl orq_orchestrator::DesignGroupSteps for Steps {
    type Stop = String;

    fn implement(
        &mut self,
        at: &orq_orchestrator::MemberAt<'_>,
    ) -> Result<Option<String>, orq_orchestrator::StoreError> {
        self.step(format!("implement {}", at.member.feature))
    }

    fn gate(
        &mut self,
        at: &orq_orchestrator::MemberAt<'_>,
    ) -> Result<Option<String>, orq_orchestrator::StoreError> {
        self.step(format!("gate {}", at.member.feature))
    }

    fn design(
        &mut self,
        at: &orq_orchestrator::MemberAt<'_>,
        done: &[String],
    ) -> Result<Option<String>, orq_orchestrator::StoreError> {
        self.calls
            .push(format!("done {} {}", at.member.feature, done.join(",")));
        self.step(format!("design {}", at.member.feature))
    }

    fn finish(&mut self) -> Result<Option<String>, orq_orchestrator::StoreError> {
        self.step("finish".to_string())
    }
}

#[test]
fn a_group_of_three_frontend_members_runs_each_members_session_and_design_loop_in_order_and_finishes_once(
) {
    let (store, root) = store("three");
    let members = [
        member("a", &["H01"]),
        member("b", &["H02"]),
        member("c", &["H03"]),
    ];
    let mut steps = Steps::default();

    let stop = orq_orchestrator::run_design_group(&store, "run-1", &members, &mut steps).unwrap();

    assert!(stop.is_none());
    let expected = [
        "implement a",
        "gate a",
        "done a ",
        "design a",
        "implement b",
        "gate b",
        "done b H01",
        "design b",
        "implement c",
        "gate c",
        "done c H01,H02",
        "design c",
        "finish",
    ];
    assert_eq!(steps.calls, expected);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_member_that_stops_stops_the_group_and_the_resume_continues_with_it_not_the_members_before() {
    let (store, root) = store("stop-resume");
    let members = [
        member("a", &["H01"]),
        member("s", &[]),
        member("b", &["H02"]),
        member("c", &["H03"]),
    ];
    let mut first = Steps {
        stop: Some("design b".to_string()),
        ..Steps::default()
    };

    let stop = orq_orchestrator::run_design_group(&store, "run-1", &members, &mut first)
        .unwrap()
        .expect("b stops the group");

    assert_eq!(
        (
            stop.index,
            stop.of,
            stop.member.as_str(),
            stop.stop.as_str()
        ),
        (3, 4, "b", "design b needs review")
    );
    let expected = [
        "implement a",
        "gate a",
        "done a ",
        "design a",
        "implement s",
        "gate s",
        "implement b",
        "gate b",
        "done b H01",
        "design b",
    ];
    assert_eq!(
        first.calls, expected,
        "the standard member s has no design loop, and c never starts"
    );

    let mut resumed = Steps::default();
    assert!(
        orq_orchestrator::run_design_group(&store, "run-1", &members, &mut resumed)
            .unwrap()
            .is_none()
    );
    let expected = [
        "gate b",
        "done b H01",
        "design b",
        "implement c",
        "gate c",
        "done c H01,H02",
        "design c",
        "finish",
    ];
    assert_eq!(
        resumed.calls, expected,
        "b's session is not run again, nor anything of a and s"
    );
    let _ = std::fs::remove_dir_all(root);
}

fn review(verdict: orq_orchestrator::DesignVerdict) -> orq_orchestrator::DesignReviewOutcome {
    let issues = match verdict {
        orq_orchestrator::DesignVerdict::ChangesRequired => vec![orq_orchestrator::ReviewIssue {
            id: "hero-size".to_string(),
            section: "H01".to_string(),
            viewport: "desktop".to_string(),
            source: "references/desktop.png".to_string(),
            expected: "a 48px heading".to_string(),
            actual: "a 32px heading".to_string(),
            evidence: vec!["round-1/desktop/index.png".to_string()],
            severity: orq_orchestrator::Severity::Major,
            status: orq_orchestrator::IssueStatus::Open,
            fix_direction: "raise the heading size".to_string(),
            selector: None,
            confidence: None,
            basis: None,
            copy: None,
            quote: None,
            token: None,
            reference: None,
            downgraded_from: None,
            needs: None,
        }],
        _ => Vec::new(),
    };
    orq_orchestrator::DesignReviewOutcome {
        verdict,
        reason: None,
        review: Some(orq_orchestrator::DesignReview {
            run_id: "run-1".to_string(),
            reference_hash: "abc".to_string(),
            capture_revision: "rev".to_string(),
            evaluation: "compared".to_string(),
            verdict,
            checks: Vec::new(),
            issues,
            suggestions: Vec::new(),
            blockers: Vec::new(),
        }),
        report: std::path::PathBuf::from("/logs/run-1/design/report.md"),
    }
}

/// A member's fake design loop: the reviews it answers, in order; each correction moves the head.
struct Loop {
    head: String,
    reviews: std::collections::VecDeque<orq_orchestrator::DesignReviewOutcome>,
    reviewed: Vec<u32>,
}

impl Loop {
    fn new(head: &str, reviews: Vec<orq_orchestrator::DesignReviewOutcome>) -> Loop {
        Loop {
            head: head.to_string(),
            reviews: reviews.into(),
            reviewed: Vec::new(),
        }
    }
}

impl orq_orchestrator::DesignLoopSteps for Loop {
    fn head(&mut self) -> Result<String, orq_orchestrator::StoreError> {
        Ok(self.head.clone())
    }

    fn review(
        &mut self,
        round: u32,
    ) -> Result<orq_orchestrator::DesignReviewOutcome, orq_orchestrator::StoreError> {
        self.reviewed.push(round);
        Ok(self.reviews.pop_front().expect("a scripted review"))
    }

    fn correct(
        &mut self,
        batch: u32,
        _: &[orq_orchestrator::ReviewIssue],
        _: &[String],
    ) -> Result<orq_orchestrator::CorrectionOutcome, orq_orchestrator::StoreError> {
        self.head = format!("{}-fix-{batch}", self.head);
        Ok(orq_orchestrator::CorrectionOutcome::Done)
    }

    fn review_code(
        &mut self,
        _: u32,
        _: &str,
    ) -> Result<orq_orchestrator::CorrectionOutcome, orq_orchestrator::StoreError> {
        Ok(orq_orchestrator::CorrectionOutcome::Done)
    }
}

#[test]
fn each_members_design_loop_has_its_own_rounds_and_correction_budget() {
    use orq_orchestrator::DesignVerdict::{ChangesRequired, Pass};
    let (store, root) = store("budget");
    let mut a = Loop::new("a", vec![review(ChangesRequired), review(ChangesRequired)]);
    let ended =
        orq_orchestrator::run_design_loop_member(&store, "run-1", Some("a"), 1, &mut a).unwrap();
    assert!(
        matches!(
            ended,
            orq_orchestrator::DesignLoopOutcome::NeedsReview { .. }
        ),
        "{ended:?}"
    );

    // b's loop starts at round 1 with the whole budget: a's correction round is not counted against it.
    let mut b = Loop::new("b", vec![review(ChangesRequired), review(Pass)]);
    let ended =
        orq_orchestrator::run_design_loop_member(&store, "run-1", Some("b"), 1, &mut b).unwrap();

    assert!(
        matches!(ended, orq_orchestrator::DesignLoopOutcome::Passed { .. }),
        "{ended:?}"
    );
    assert_eq!(b.reviewed, [1, 2]);
    let events = store.events("run-1").unwrap();
    let of_b = events
        .iter()
        .filter(|e| e.kind == "design_review" && e.payload["member"] == "b")
        .count();
    assert_eq!(of_b, 2, "b's reviews are recorded as b's");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_members_design_folder_has_the_frozen_package_and_the_earlier_members_components_as_done() {
    let (_, root) = store("member-logs");
    let logs = root.join("logs");
    let design = logs.join("design");
    std::fs::create_dir_all(design.join("package")).unwrap();
    std::fs::create_dir_all(design.join("round-1")).unwrap();
    std::fs::write(design.join("manifest.json"), "{}").unwrap();
    std::fs::write(
        design.join("package/DESIGN.md"),
        "### H01 Hero\n### H02 Process\n",
    )
    .unwrap();
    std::fs::write(design.join("round-1/report.md"), "an earlier round").unwrap();
    let scope = orq_orchestrator::DesignScope {
        feature: "a".to_string(),
        package: "home".to_string(),
        components: Some(vec!["H01".to_string()]),
        done: Vec::new(),
    };
    std::fs::write(
        design.join("scope.json"),
        serde_json::to_vec(&scope).unwrap(),
    )
    .unwrap();

    let member_logs = orq_orchestrator::member_design_logs(
        &logs,
        "b",
        &["H02".to_string()],
        &["H01".to_string()],
    )
    .unwrap();

    let scope = orq_orchestrator::frozen_design_scope(&member_logs).expect("the member's scope");
    assert_eq!(scope.feature, "b");
    assert_eq!(scope.package, "home");
    assert_eq!(scope.components, Some(vec!["H02".to_string()]));
    assert_eq!(
        scope.done,
        ["H01"],
        "the components the members before it built are done"
    );
    let copied = member_logs.join("design");
    assert!(copied.join("manifest.json").is_file() && copied.join("package/DESIGN.md").is_file());
    assert!(
        !copied.join("round-1").exists(),
        "no round of another member is copied"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn members_naming_different_packages_are_refused_at_start() {
    let repo = GitFixture::new();
    git(
        repo.root(),
        &["remote", "add", "origin", "git@github.com:o/r.git"],
    );
    write_ledger(
        repo.root(),
        &[
            LedgerEntry::new("hero", "pending")
                .description("Design package: home\nDesign components: H01\n"),
            LedgerEntry::new("cart", "pending")
                .description("Design package: checkout\nDesign components: C01\n"),
        ],
    );
    mark_frontend(&repo, "hero");
    mark_frontend(&repo, "cart");

    let refused = check_group(&repo, "hero,cart");

    assert!(!refused.status.success(), "{}", text(&refused.stdout));
    let stderr = text(&refused.stderr);
    assert!(
        stderr.contains("hero") && stderr.contains("home"),
        "{stderr}"
    );
    assert!(
        stderr.contains("cart") && stderr.contains("checkout"),
        "{stderr}"
    );
    assert!(stderr.contains("one design package"), "{stderr}");
}

/// A design group `std,fa,fb` whose frontend members fa (H01) and fb (H02) share the package `designs/home/`,
/// whose page never starts, so a member's design review is blocked before any reviewer. Each implement
/// session (a fake claude) adds a line to `work.txt`, commits, and writes its task to `$HOME/sessions`.
struct Group {
    repo: GitFixture,
    home: std::path::PathBuf,
}

fn group(tag: &str) -> Group {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    let home = orq_test_support::process_temp_dir()
        .join(format!("orq-design-group-run-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("bin")).unwrap();
    let git_q = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo.root())
            .args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", text(&out.stderr));
    };
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    std::fs::write(
        repo.root().join(".orq/run.toml"),
        "[design]\nstart_command = [\"false\"]\nmode = \"infer\"\n\n[design.viewports]\ndesktop = \"400x300\"\ntablet = \"300x400\"\nmobile = \"200x400\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(repo.root().join("designs/home/references")).unwrap();
    std::fs::write(repo.root().join("designs/home/DESIGN.md"), DESIGN_MD).unwrap();
    for (name, width, height) in [
        ("desktop", 400, 300),
        ("tablet", 300, 400),
        ("mobile", 200, 400),
    ] {
        std::fs::write(
            repo.root()
                .join(format!("designs/home/references/{name}.png")),
            png(width, height),
        )
        .unwrap();
    }
    write_ledger(
        repo.root(),
        &[
            LedgerEntry::new("std", "pending"),
            LedgerEntry::new("fa", "pending")
                .description("Design package: home\nDesign components: H01\n"),
            LedgerEntry::new("fb", "pending")
                .description("Design package: home\nDesign components: H02\n"),
        ],
    );
    git_q(&["add", "-A"]);
    git_q(&["commit", "--quiet", "-m", "design package"]);
    git_q(&["branch", "-M", "main"]);
    git_q(&["push", "--quiet", "origin", "main"]);
    git_q(&[
        "remote",
        "set-url",
        "origin",
        &format!("forge.example:{}", remote.display()),
    ]);
    let ssh = home.join("ssh");
    std::fs::write(
        &ssh,
        "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    // The preparer keeps the package's components.
    let prepared = serde_json::json!({
        "files": [
            {"path": "DESIGN.md", "content": DESIGN_MD},
            {"path": "RESPONSIVE.md", "content": "Stack."},
            {"path": "tokens-mapping.md", "content": "none"},
        ],
        "conflicts": [], "inferred": [], "needed_files": [],
    });
    std::fs::write(home.join("preparer.json"), prepared.to_string()).unwrap();
    Group { repo, home }
}

const DESIGN_MD: &str = "# Home\n\n### H01 Hero\n\nThe hero.\n\n### H02 Process\n\nThe process.\n";

impl Group {
    fn command(&self) -> Command {
        let mut command = orq_test_support::sandboxed_command(env!("CARGO_BIN_EXE_orq"));
        command
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("XDG_STATE_HOME", self.home.join("state"))
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .env("ORQ_CHROMIUM", self.home.join("no-chromium"))
            .env("ORQ_FORGE", "gh")
            .env("GIT_SSH_COMMAND", self.home.join("ssh"))
            .env_remove("HERDR_ACTIVE_PANE_CWD")
            .env_remove("TDD_FEATURES_PATH");
        command
    }

    /// The project's design reviewer is a pi that is never reached, and fa and fb are frontend features.
    fn prepare(&self) {
        let roots = orq_core::storage::StorageRoots::resolve(&|name| match name {
            "XDG_DATA_HOME" => Some(self.home.join("data").display().to_string()),
            "XDG_CONFIG_HOME" => Some(self.home.join("config").display().to_string()),
            "XDG_STATE_HOME" => Some(self.home.join("state").display().to_string()),
            "XDG_CACHE_HOME" => Some(self.home.join("cache").display().to_string()),
            _ => None,
        })
        .unwrap();
        roots.create_all().unwrap();
        let mut store = orq_core::store::Store::open(&roots.database()).unwrap();
        let root = std::fs::canonicalize(self.repo.root()).unwrap();
        let revision = store.project_settings(&root).unwrap().revision;
        let settings = orq_core::project_settings::ProjectSettings {
            design_reviewer: Some(orq_core::project_settings::ProjectAgent {
                harness: "pi".to_string(),
                model: "openai/gpt-6.1-sol".to_string(),
                effort: Some("medium".to_string()),
            }),
            ..Default::default()
        };
        store
            .save_project_settings(&root, revision, &settings)
            .unwrap();
        drop(store);
        for feature in ["fa", "fb"] {
            let kind = self
                .command()
                .args(["feature", "kind", "--repo"])
                .arg(self.repo.root())
                .args(["--id", feature, "frontend"])
                .output()
                .unwrap();
            assert!(kind.status.success(), "{}", text(&kind.stderr));
        }
    }

    fn orq_run(
        &self,
        agent: &orq_test_support::FakeAgent,
        provider: &orq_test_support::FakeProvider,
        args: &[&str],
    ) -> Output {
        let bin = self.home.join("bin");
        let claude = format!(
            "#!/bin/sh\nprintf '%s\\n----\\n' \"$*\" >> \"$HOME/sessions\"\ndate +%s%N >> work.txt\nexec '{}' \"$@\"\n",
            agent.path().display()
        );
        std::fs::write(bin.join("claude"), claude).unwrap();
        std::fs::set_permissions(
            bin.join("claude"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let pi = "#!/bin/sh\nif [ \"$1\" = --list-models ]; then printf 'provider  model  context  max-out  thinking  images\\nopenai  gpt-6.1-sol  400K  128K  yes  yes\\n'; exit 0; fi\n\
                  for a; do last=$a; done\n\
                  case \"$last\" in *'You are the design preparer'*) ;; *) exit 1 ;; esac\n\
                  cat \"$HOME/preparer.json\"\n";
        std::fs::write(bin.join("pi"), pi).unwrap();
        std::fs::set_permissions(
            bin.join("pi"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let mut dirs = vec![bin, provider.bin_dir()];
        dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        self.command()
            .args(args)
            .env("PATH", std::env::join_paths(dirs).unwrap())
            .output()
            .unwrap()
    }

    /// The tasks of the implement sessions so far, in order.
    fn sessions(&self) -> Vec<String> {
        let text = std::fs::read_to_string(self.home.join("sessions")).unwrap_or_default();
        text.split("\n----\n")
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Every event of `run_id`, as JSON.
    fn events(&self, run_id: &str) -> Vec<serde_json::Value> {
        let text = std::fs::read_to_string(
            self.home
                .join("state/orq/runs")
                .join(run_id)
                .join("events.jsonl"),
        )
        .unwrap_or_default();
        text.lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .collect()
    }

    fn run_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = std::fs::read_dir(self.home.join("state/orq/runs"))
            .unwrap()
            .map(|d| d.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        ids.sort();
        ids
    }
}

/// The group's member steps an event list records: `(member, step)`.
fn member_steps(events: &[serde_json::Value]) -> Vec<(String, String)> {
    events
        .iter()
        .filter(|e| e["type"] == "design_group.member_step")
        .map(|e| {
            (
                e["payload"]["member"].as_str().unwrap().to_string(),
                e["payload"]["step"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn pairs(steps: &[(&str, &str)]) -> Vec<(String, String)> {
    steps
        .iter()
        .map(|(m, s)| (m.to_string(), s.to_string()))
        .collect()
}

#[test]
fn a_design_group_runs_each_members_session_and_stops_at_the_member_needing_review_and_resumes_with_it(
) {
    let w = group("stop-resume");
    w.prepare();
    let agent = orq_test_support::FakeAgent::builder()
        .commit("feat: a member")
        .build();
    let provider = orq_test_support::FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .build();
    let root = w.repo.root().to_str().unwrap().to_string();

    let first = w.orq_run(
        &agent,
        &provider,
        &[
            "run",
            "--repo",
            &root,
            "--group",
            "std,fa,fb",
            "--remote",
            "origin",
            "--target",
            "main",
            "--no-ci-check",
        ],
    );

    let all = format!("{}{}", text(&first.stdout), text(&first.stderr));
    assert!(!first.status.success(), "{all}");
    assert!(
        all.contains("member 2 of 3: fa: the design review is blocked"),
        "the run says which member stopped it and why: {all}"
    );
    assert!(
        all.contains("member 2 of 3: fa, design round 1"),
        "progress names the member: {all}"
    );
    let sessions = w.sessions();
    assert_eq!(
        sessions.len(),
        2,
        "std's and fa's sessions, never fb's: {sessions:#?}\n{all}"
    );
    assert!(
        sessions[0].contains("/implement-and-review std ")
            && !sessions[0].contains("\"feature\":\"fa\""),
        "a member's session has only its own task: {}",
        sessions[0]
    );
    assert!(
        sessions[1].contains("/implement-and-review fa ")
            && !sessions[1].contains("\"feature\":\"fb\""),
        "{}",
        sessions[1]
    );
    let run_id = w.run_ids().pop().unwrap();
    let events = w.events(&run_id);
    let packages = events
        .iter()
        .filter(|e| e["type"] == "stage.started" && e["stage"] == "design_package")
        .count();
    assert_eq!(packages, 1, "the package is frozen once: {events:#?}");
    let expected = pairs(&[
        ("std", "implement"),
        ("std", "gate"),
        ("fa", "implement"),
        ("fa", "gate"),
        ("fa", "design"),
    ]);
    assert_eq!(
        member_steps(&events),
        expected,
        "std has no design loop and fb never starts"
    );
    assert!(
        !events.iter().any(|e| e["stage"] == "push"),
        "a stopped group delivers nothing"
    );
    let scope = std::fs::read_to_string(
        w.home
            .join("state/orq/logs")
            .join(&run_id)
            .join("members/fa/design/scope.json"),
    )
    .unwrap();
    assert!(
        scope.contains("H01") && !scope.contains("H02"),
        "fa's review is scoped to its components: {scope}"
    );

    // orq dies after fa's design step: the run has no ending, and is resumed.
    let log = w
        .home
        .join("state/orq/runs")
        .join(&run_id)
        .join("events.jsonl");
    let kept: Vec<String> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .filter(|l| !l.contains("\"run.completed\""))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&log, kept.concat()).unwrap();
    let resumed = w.orq_run(
        &agent,
        &provider,
        &[
            "run",
            "resume",
            &run_id,
            "--yes",
            "--no-ci-check",
            "--repo",
            &root,
        ],
    );

    let all = format!("{}{}", text(&resumed.stdout), text(&resumed.stderr));
    assert_eq!(
        w.sessions().len(),
        2,
        "no member's session runs again: {all}"
    );
    let resumed_id = w
        .run_ids()
        .into_iter()
        .find(|id| *id != run_id)
        .unwrap_or_else(|| panic!("no resumed run: {all}"));
    assert_eq!(
        member_steps(&w.events(&resumed_id)),
        pairs(&[("fa", "gate"), ("fa", "design")]),
        "the resume continues with fa, not std: {all}"
    );
    assert!(all.contains("member 2 of 3: fa"), "{all}");
    let _ = std::fs::remove_dir_all(&w.home);
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// A zlib stream of `n` zero bytes in stored (uncompressed) blocks.
fn zlib_zeros(n: usize) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut left = n;
    loop {
        let block = left.min(65_535);
        left -= block;
        out.push(if left == 0 { 1 } else { 0 });
        out.extend_from_slice(&(block as u16).to_le_bytes());
        out.extend_from_slice(&(!(block as u16)).to_le_bytes());
        out.extend(std::iter::repeat_n(0, block));
        if left == 0 {
            break;
        }
    }
    let adler = (((n % 65521) as u32) << 16) | 1;
    out.extend_from_slice(&adler.to_be_bytes());
    out
}

/// A valid black PNG of `width` × `height`.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(
        &mut out,
        b"IDAT",
        &zlib_zeros(height as usize * (1 + 3 * width as usize)),
    );
    png_chunk(&mut out, b"IEND", &[]);
    out
}
