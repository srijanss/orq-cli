//! A group's members wait for delivery in a status of their own: when a session of the run completes a
//! member, orq sets it `awaiting_delivery` on the run's branch (`heldOn`, `heldBy`), which the TDD server
//! accepts as a met dependency on that branch, so each later member's session starts; the run's delivery
//! sets every one of them `completed` at once. A resume, a commit on top or a discard never makes one
//! `pending` again by mistake.

use std::path::PathBuf;
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

const DESIGN_MD: &str = "# Home\n\n### H01 Hero\n\nThe hero.\n";

/// The design group `a,b,c`: b depends on a, c on b; `frontend` (H01 of `designs/home/`) is a frontend member
/// whose page never starts, so its design review is blocked and the group stops after its gate. `extra`, not
/// in the group, is a feature a's session starts when `$HOME/extra` exists. Each implement session is a fake
/// claude that first checks, as the TDD server's `init_feature` does (outside-in-tdd-mcp 0.5.0), that each of
/// its feature's dependencies is `completed`, or `awaiting_delivery` held on the branch the session is on
/// (writing `<feature> <dependency> <status>` to `$HOME/seen`, and refusing with `$HOME/refused` when one is
/// not), then marks its feature `completed` and commits `<feature>.txt`.
struct Group {
    repo: GitFixture,
    home: PathBuf,
}

fn group(tag: &str, frontend: &str, run_toml: &str) -> Group {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    let home = orq_test_support::process_temp_dir().join(format!("orq-group-await-{}-{tag}", std::process::id()));
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
        format!("explain_failures = false\n{}\n[design]\nstart_command = [\"false\"]\nmode = \"infer\"\n\n[design.viewports]\ndesktop = \"400x300\"\ntablet = \"300x400\"\nmobile = \"200x400\"\n", run_toml.replace("{home}", &home.display().to_string())),
    )
    .unwrap();
    std::fs::create_dir_all(repo.root().join("designs/home/references")).unwrap();
    std::fs::write(repo.root().join("designs/home/DESIGN.md"), DESIGN_MD).unwrap();
    for (name, width, height) in [("desktop", 400, 300), ("tablet", 300, 400), ("mobile", 200, 400)] {
        std::fs::write(repo.root().join(format!("designs/home/references/{name}.png")), png(width, height)).unwrap();
    }
    let member = |name: &str, deps: &[&str]| {
        let entry = LedgerEntry::new(name, "pending").depends_on(deps);
        if name == frontend {
            entry.description("Design package: home\nDesign components: H01\n")
        } else {
            entry
        }
    };
    let entries = vec![member("a", &[]), member("b", &["a"]), member("c", &["b"]), LedgerEntry::new("extra", "pending"), LedgerEntry::new("d", "pending").depends_on(&["a"])];
    write_ledger(repo.root(), &entries);
    git_q(&["add", "-A"]);
    git_q(&["commit", "--quiet", "-m", "design package"]);
    git_q(&["branch", "-M", "main"]);
    git_q(&["push", "--quiet", "origin", "main"]);
    git_q(&["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let ssh = home.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let prepared = serde_json::json!({
        "files": [
            {"path": "DESIGN.md", "content": DESIGN_MD},
            {"path": "RESPONSIVE.md", "content": "Stack."},
            {"path": "tokens-mapping.md", "content": "none"},
        ],
        "conflicts": [], "inferred": [], "needed_files": [],
    });
    std::fs::write(home.join("preparer.json"), prepared.to_string()).unwrap();
    let g = Group { repo, home };
    g.prepare(frontend);
    g
}

/// The value of the member `$3` of the entry `$1` in the ledger `$2`, as an awk program over its
/// one-member-per-line layout.
const FIELD_OF: &str = "awk -v n=\"$1\" -v k=\"$3\" '/\"featureName\":/ { cur = index($0, \"\\\"featureName\\\": \\\"\" n \"\\\"\") > 0 } index($0, \"\\\"\" k \"\\\":\") && cur { sub(/.*\": \"/, \"\"); sub(/\".*/, \"\"); print; exit }' \"$2\"";

/// Sets `$1` to `$3` in the ledger `$2`, keeping the file.
const SET_STATUS: &str = "awk -v n=\"$1\" -v s=\"$3\" '/\"featureName\":/ { cur = index($0, \"\\\"featureName\\\": \\\"\" n \"\\\"\") > 0 } /\"status\":/ && cur { sub(/\"status\": \"[a-z_]+\"/, \"\\\"status\\\": \\\"\" s \"\\\"\") } { print }' \"$2\" > \"$2.tmp\" && cat \"$2.tmp\" > \"$2\" && rm \"$2.tmp\"";

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
            .env_remove("TDD_FEATURES_PATH")
            .env_remove("ORQ_GATE")
            .env_remove("ORQ_MEMBER_GATE");
        command
    }

    /// The project's design reviewer is a pi that is never reached, and `frontend` is a frontend feature.
    fn prepare(&self, frontend: &str) {
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
        store.save_project_settings(&root, revision, &settings).unwrap();
        drop(store);
        let kind = self.command().args(["feature", "kind", "--repo"]).arg(self.repo.root()).args(["--id", frontend, "frontend"]).output().unwrap();
        assert!(kind.status.success(), "{}", text(&kind.stderr));
    }

    /// `orq <args>` with the fake claude (dependency check, then `agent`) and a pi that only prepares the package.
    fn orq(&self, agent: &FakeAgent, provider: &FakeProvider, args: &[&str]) -> Output {
        let bin = self.home.join("bin");
        let ledger = self.repo.root().join(".tdd-features.json");
        let claude = format!(
            "#!/bin/sh\nfield_of() {{ {FIELD_OF}; }}\nset_status() {{ {SET_STATUS}; }}\n\
             ledger='{ledger}'\n\
             printf '%s\\n----\\n' \"$*\" >> \"$HOME/sessions\"\n\
             date +%s%N >> work.txt\n\
             case \"$*\" in\n\
               *'/implement-and-review a '*) f=a ;;\n\
               *'/implement-and-review b '*) f=b ;;\n\
               *'/implement-and-review c '*) f=c ;;\n\
               *) exec '{agent}' \"$@\" ;;\n\
             esac\n\
             branch=$(git rev-parse --abbrev-ref HEAD)\n\
             deps=$(awk -v n=\"$f\" '/\"dependsOn\": \\[\\]/ {{ list = \"\"; next }} /\"dependsOn\":/ {{ on = 1; list = \"\"; next }} on && /\\]/ {{ on = 0; next }} on {{ gsub(/[\" ,]/, \"\"); list = list \" \" $0; next }} /\"featureName\":/ && index($0, \"\\\"featureName\\\": \\\"\" n \"\\\"\") > 0 {{ print list; exit }}' \"$ledger\")\n\
             for d in $deps; do\n\
               s=$(field_of \"$d\" \"$ledger\" status)\n\
               held=$(field_of \"$d\" \"$ledger\" heldOn)\n\
               echo \"$f $d $s\" >> \"$HOME/seen\"\n\
               if [ \"$s\" = awaiting_delivery ] && [ \"$held\" = \"$branch\" ]; then continue; fi\n\
               if [ \"$s\" != completed ]; then echo \"Feature '$f' depends on feature(s) not yet completed: $d\" >> \"$HOME/refused\"; exit 3; fi\n\
             done\n\
             if [ \"$f\" = c ] && [ -f \"$HOME/fail-c\" ]; then exit 3; fi\n\
             if [ \"$f\" = a ] && [ -f \"$HOME/extra\" ]; then set_status extra \"$ledger\" in_progress; printf '{{\"event\":\"init_feature\",\"featureName\":\"extra\"}}\\n' >> .tdd-session.log; fi\n\
             date > \"$f.txt\"\n\
             set_status \"$f\" \"$ledger\" completed\n\
             exec '{agent}' \"$@\"\n",
            ledger = ledger.display(),
            agent = agent.path().display(),
        );
        std::fs::write(bin.join("claude"), claude).unwrap();
        std::fs::set_permissions(bin.join("claude"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let pi = "#!/bin/sh\nif [ \"$1\" = --list-models ]; then printf 'provider  model  context  max-out  thinking  images\\nopenai  gpt-6.1-sol  400K  128K  yes  yes\\n'; exit 0; fi\n\
                  for a; do last=$a; done\n\
                  case \"$last\" in *'You are the design preparer'*) ;; *) exit 1 ;; esac\n\
                  cat \"$HOME/preparer.json\"\n";
        std::fs::write(bin.join("pi"), pi).unwrap();
        std::fs::set_permissions(bin.join("pi"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let mut dirs = vec![bin, provider.bin_dir()];
        dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        self.command().args(args).env("PATH", std::env::join_paths(dirs).unwrap()).output().unwrap()
    }

    fn root(&self) -> String {
        self.repo.root().to_str().unwrap().to_string()
    }

    fn run_group(&self, agent: &FakeAgent, provider: &FakeProvider) -> Output {
        self.orq(agent, provider, &["run", "--repo", &self.root(), "--group", "a,b,c", "--remote", "origin", "--target", "main", "--no-ci-check"])
    }

    fn resume(&self, run_id: &str, agent: &FakeAgent, provider: &FakeProvider) -> Output {
        self.orq(agent, provider, &["run", "resume", run_id, "--yes", "--no-ci-check", "--repo", &self.root()])
    }

    /// What `$HOME/<name>` holds, one entry per line.
    fn lines_of(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.home.join(name)).unwrap_or_default().lines().map(str::to_string).collect()
    }

    /// The entry the project ledger records for `name`.
    fn entry(&self, name: &str) -> serde_json::Value {
        let ledger: serde_json::Value = serde_json::from_slice(&std::fs::read(self.repo.root().join(".tdd-features.json")).unwrap()).unwrap();
        ledger.as_array().unwrap().iter().find(|e| e["featureName"] == name).unwrap().clone()
    }

    fn status_of(&self, name: &str) -> String {
        self.entry(name)["status"].as_str().unwrap().to_string()
    }

    /// Every event of `run_id`, as JSON.
    fn events(&self, run_id: &str) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.home.join("state/orq/runs").join(run_id).join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .collect()
    }

    fn run_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = std::fs::read_dir(self.home.join("state/orq/runs")).map(|dir| dir.map(|d| d.unwrap().file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        ids.sort();
        ids
    }

    /// The run `run_id`'s record, as JSON.
    fn record(&self, run_id: &str) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(self.home.join("state/orq/runs").join(run_id).join("run.json")).unwrap()).unwrap()
    }

    /// Marks `member` (the `index`th of 3) passed and the run `run_id` unended, as if it had got through its
    /// design loop before orq died.
    fn pass(&self, run_id: &str, member: &str, index: u64) {
        let log = self.home.join("state/orq/runs").join(run_id).join("events.jsonl");
        let lines: Vec<String> = std::fs::read_to_string(&log).unwrap().lines().filter(|l| !l.contains("\"run.completed\"")).map(str::to_string).collect();
        let step = lines.iter().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()).find(|e| e["type"] == "design_group.member_step").unwrap();
        let mut kept: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let mut passed = step.clone();
        passed["type"] = "design_group.member_passed".into();
        passed["event_id"] = format!("{}-passed-{member}", step["event_id"].as_str().unwrap()).into();
        passed["payload"] = serde_json::json!({"member": member, "index": index, "of": 3, "step": "passed"});
        kept.push_str(&format!("{passed}\n"));
        std::fs::write(&log, kept).unwrap();
    }
}

/// The payloads of `kind` among `events`, in order.
fn payloads(events: &[serde_json::Value], kind: &str) -> Vec<serde_json::Value> {
    events.iter().filter(|e| e["type"] == kind).map(|e| e["payload"].clone()).collect()
}

fn delivering() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], "https://github.example/o/r/pull/7\n")
        .reply(&["pr", "view"], "{\"number\":7,\"url\":\"https://github.example/o/r/pull/7\"}\n")
        .build()
}

#[test]
fn a_chain_of_three_members_awaits_delivery_on_the_runs_branch_and_delivery_completes_all_of_them_at_once() {
    let w = group("chain", "c", "");
    std::fs::write(w.home.join("extra"), "").unwrap();
    let agent = FakeAgent::builder().commit("feat: a member").build();
    let provider = delivering();

    let first = w.run_group(&agent, &provider);

    let all = format!("{}{}", text(&first.stdout), text(&first.stderr));
    assert!(all.contains("member 3 of 3: c: the design review is blocked"), "every member ran, c stops at its design review: {all}");
    assert!(w.lines_of("refused").is_empty(), "no session was refused: {:?}\n{all}", w.lines_of("refused"));
    assert_eq!(w.lines_of("seen"), vec!["b a awaiting_delivery", "c b awaiting_delivery"], "{all}");
    let run_id = w.run_ids().pop().unwrap();
    let branch = w.record(&run_id)["branch"].as_str().unwrap().to_string();
    for name in ["a", "b", "c"] {
        let entry = w.entry(name);
        assert_eq!(entry["status"], "awaiting_delivery", "{name} awaits delivery: {all}");
        assert_eq!(entry["heldOn"], branch.as_str(), "{name}");
        assert_eq!(entry["heldBy"], run_id.as_str(), "{name}");
    }
    let events = w.events(&run_id);
    assert_eq!(
        payloads(&events, "ledger.awaiting_delivery"),
        ["a", "b", "c"].map(|name| serde_json::json!({"features": [name], "branch": branch})).to_vec(),
    );
    assert!(payloads(&events, "ledger.held_until_delivery").is_empty() && payloads(&events, "ledger.released_for_member").is_empty());
    // Not a met dependency for a feature outside the run.
    let other = w.orq(&agent, &provider, &["run", "--repo", &w.root(), "--feature", "d", "--remote", "origin", "--target", "main", "--check"]);
    assert!(!other.status.success(), "{}", text(&other.stdout));
    assert!(text(&other.stderr).contains("a (awaiting_delivery)") || text(&other.stdout).contains("a (awaiting_delivery)"), "{}{}", text(&other.stdout), text(&other.stderr));

    w.pass(&run_id, "c", 3);
    let resumed = w.resume(&run_id, &agent, &provider);

    let all = format!("{}{}", text(&resumed.stdout), text(&resumed.stderr));
    assert!(resumed.status.success(), "{all}");
    for name in ["a", "b", "c", "extra"] {
        assert_eq!(w.status_of(name), "completed", "delivery completes {name}: {all}");
    }
    let resumed_id = w.run_ids().into_iter().find(|id| *id != run_id).unwrap();
    let completed = payloads(&w.events(&resumed_id), "ledger.completed_by_run");
    assert_eq!(completed.len(), 1, "one save: {completed:?}");
    let mut names: Vec<String> = completed[0]["features"].as_array().unwrap().iter().map(|n| n.as_str().unwrap().to_string()).collect();
    names.sort();
    assert_eq!(names, ["a", "b", "c", "extra"]);
    let _ = std::fs::remove_dir_all(&w.home);
}

impl Group {
    /// The worktree of the run `run_id`.
    fn worktree(&self, run_id: &str) -> PathBuf {
        PathBuf::from(self.record(run_id)["worktree"].as_str().unwrap())
    }

    /// Rewrites the project ledger with `change` applied to its entries, laid out one member per line.
    fn rewrite_ledger(&self, change: impl Fn(&mut serde_json::Map<String, serde_json::Value>)) {
        let path = self.repo.root().join(".tdd-features.json");
        let mut ledger: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for entry in ledger.as_array_mut().unwrap() {
            change(entry.as_object_mut().unwrap());
        }
        std::fs::write(&path, serde_json::to_string_pretty(&ledger).unwrap() + "\n").unwrap();
    }
}

/// Commits `file` in `dir`.
fn commit_in(dir: &std::path::Path, file: &str) {
    std::fs::write(dir.join(file), "on top\n").unwrap();
    for args in [&["add", file][..], &["commit", "--quiet", "-m", "on top"][..]] {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", text(&out.stderr));
    }
}

#[test]
fn a_run_stopped_at_bs_design_review_and_resumed_after_a_commit_on_top_keeps_a_awaiting_and_starts_c() {
    let w = group("on-top", "b", "");
    let agent = FakeAgent::builder().commit("feat: a member").build();
    let provider = delivering();

    let first = w.run_group(&agent, &provider);

    let all = format!("{}{}", text(&first.stdout), text(&first.stderr));
    assert!(all.contains("member 2 of 3: b: the design review is blocked"), "{all}");
    let run_id = w.run_ids().pop().unwrap();
    assert_eq!(w.status_of("a"), "awaiting_delivery", "{all}");
    assert_eq!(w.status_of("b"), "awaiting_delivery", "{all}");
    assert_eq!(w.status_of("c"), "pending", "{all}");
    // B's design round committed on top of what A's session left.
    commit_in(&w.worktree(&run_id), "design-round.txt");
    w.pass(&run_id, "b", 2);

    let resumed = w.resume(&run_id, &agent, &provider);

    let all = format!("{}{}", text(&resumed.stdout), text(&resumed.stderr));
    assert!(w.lines_of("refused").is_empty(), "no session was refused: {:?}\n{all}", w.lines_of("refused"));
    assert_eq!(w.lines_of("seen"), vec!["b a awaiting_delivery", "c b awaiting_delivery"], "c started with b awaiting: {all}");
    assert!(resumed.status.success(), "{all}");
    let resumed_id = w.run_ids().into_iter().find(|id| *id != run_id).unwrap();
    let handed_back: Vec<serde_json::Value> = payloads(&w.events(&resumed_id), "ledger.handed_back");
    assert!(handed_back.iter().all(|p| !p["features"].as_array().unwrap().iter().any(|n| n == "a" || n == "b")), "a and b were never handed back: {handed_back:?}");
    for name in ["a", "b", "c"] {
        assert_eq!(w.status_of(name), "completed", "{name}: {all}");
    }
    let _ = std::fs::remove_dir_all(&w.home);
}

#[test]
fn a_resumed_run_takes_over_what_it_awaits_and_a_discard_puts_it_back_to_pending() {
    let w = group("discard", "b", "");
    let agent = FakeAgent::builder().commit("feat: a member").build();
    let provider = delivering();
    let first = w.run_group(&agent, &provider);
    let all = format!("{}{}", text(&first.stdout), text(&first.stderr));
    let run_id = w.run_ids().pop().unwrap();
    assert_eq!(w.status_of("a"), "awaiting_delivery", "{all}");

    // Resumed past b's design review, it fails in c's session: the resumed run now holds a and b.
    w.pass(&run_id, "b", 2);
    std::fs::write(w.home.join("fail-c"), "").unwrap();
    let again = w.resume(&run_id, &agent, &provider);
    let all = format!("{}{}", text(&again.stdout), text(&again.stderr));
    assert!(!again.status.success(), "{all}");
    let resumed_id = w.run_ids().into_iter().find(|id| *id != run_id).unwrap();
    for name in ["a", "b"] {
        assert_eq!(w.status_of(name), "awaiting_delivery", "{name}: {all}");
        assert_eq!(w.entry(name)["heldBy"], resumed_id.as_str(), "{name} is held by the resumed run: {all}");
    }

    let discard = w.orq(&agent, &provider, &["run", "discard", &resumed_id, "--yes", "--repo", &w.root()]);

    let all = format!("{}{}", text(&discard.stdout), text(&discard.stderr));
    assert!(discard.status.success(), "{all}");
    for name in ["a", "b", "c"] {
        assert_eq!(w.status_of(name), "pending", "{name} goes back to pending: {all}");
    }
    let _ = std::fs::remove_dir_all(&w.home);
}

#[test]
fn a_run_from_before_the_status_has_its_built_members_set_awaiting_on_resume_and_the_next_member_starts() {
    let w = group("legacy", "b", "");
    let agent = FakeAgent::builder().commit("feat: a member").build();
    let provider = delivering();
    let first = w.run_group(&agent, &provider);
    let all = format!("{}{}", text(&first.stdout), text(&first.stderr));
    let run_id = w.run_ids().pop().unwrap();
    assert!(all.contains("member 2 of 3: b: the design review is blocked"), "{all}");
    // As an older orq left it: a held in_progress, b handed back to pending, neither awaiting.
    w.rewrite_ledger(|entry| {
        entry.remove("heldOn");
        entry.remove("heldBy");
        match entry["featureName"].as_str().unwrap() {
            "a" => entry.insert("status".into(), "in_progress".into()),
            "b" => entry.insert("status".into(), "pending".into()),
            _ => None,
        };
    });
    w.pass(&run_id, "b", 2);

    let resumed = w.resume(&run_id, &agent, &provider);

    let all = format!("{}{}", text(&resumed.stdout), text(&resumed.stderr));
    assert!(w.lines_of("refused").is_empty(), "no session was refused: {:?}\n{all}", w.lines_of("refused"));
    assert_eq!(w.lines_of("seen"), vec!["b a awaiting_delivery", "c b awaiting_delivery"], "{all}");
    let resumed_id = w.run_ids().into_iter().find(|id| *id != run_id).unwrap();
    let branch = w.record(&resumed_id)["branch"].as_str().unwrap().to_string();
    assert!(
        payloads(&w.events(&resumed_id), "ledger.awaiting_delivery").contains(&serde_json::json!({"features": ["a", "b"], "branch": branch})),
        "a and b were set awaiting first: {all}"
    );
    assert!(resumed.status.success(), "{all}");
    for name in ["a", "b", "c"] {
        assert_eq!(w.status_of(name), "completed", "{name}: {all}");
    }
    let _ = std::fs::remove_dir_all(&w.home);
}

#[test]
fn set_status_refuses_awaiting_delivery() {
    let w = group("set-status", "c", "");
    let agent = FakeAgent::builder().build();
    let provider = delivering();

    let out = w.orq(&agent, &provider, &["feature", "set-status", "--repo", &w.root(), "--id", "a", "--status", "awaiting_delivery"]);

    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("only a run sets it"), "{}", text(&out.stderr));
    assert_eq!(w.status_of("a"), "pending");
    let _ = std::fs::remove_dir_all(&w.home);
}

#[test]
fn a_group_run_is_refused_before_anything_is_recorded_when_the_lock_pins_an_older_tdd_server() {
    let w = group("old-server", "c", "");
    std::fs::write(w.repo.root().join(".mcpctl.lock"), "[[mcp]]\nname = \"project-mcp\"\nversion = \"0.9.0\"\n\n[[mcp]]\nname = \"outside-in-tdd-mcp\"\nversion = \"0.4.3\"\n").unwrap();
    let agent = FakeAgent::builder().commit("feat: a member").build();
    let provider = delivering();

    let out = w.run_group(&agent, &provider);

    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("update outside-in-tdd-mcp to 0.5.0 (mcpctl) for group runs"), "{}", text(&out.stderr));
    assert!(w.run_ids().is_empty(), "nothing was recorded: {:?}", w.run_ids());
    assert_eq!(w.status_of("a"), "pending");
    assert!(w.lines_of("sessions").is_empty());

    // 0.5.0 lets it start.
    std::fs::write(w.repo.root().join(".mcpctl.lock"), "[[mcp]]\nname = \"outside-in-tdd-mcp\"\nversion = \"0.5.0\"\n").unwrap();
    let started = w.orq(&agent, &provider, &["run", "--repo", &w.root(), "--group", "a,b,c", "--remote", "origin", "--target", "main", "--check"]);
    assert!(!text(&started.stderr).contains("update outside-in-tdd-mcp"), "{}", text(&started.stderr));
    let _ = std::fs::remove_dir_all(&w.home);
}

#[test]
fn a_group_run_is_refused_when_the_lock_pins_a_prerelease_of_the_tdd_server_0_5_0_and_started_from_0_5_1() {
    let w = group("prerelease-server", "c", "");
    let agent = FakeAgent::builder().commit("feat: a member").build();
    let provider = delivering();
    let check = |version: &str| {
        std::fs::write(w.repo.root().join(".mcpctl.lock"), format!("[[mcp]]\nname = \"outside-in-tdd-mcp\"\nversion = \"{version}\"\n")).unwrap();
        let out = w.orq(&agent, &provider, &["run", "--repo", &w.root(), "--group", "a,b,c", "--remote", "origin", "--target", "main", "--check"]);
        text(&out.stderr).contains("update outside-in-tdd-mcp to 0.5.0 (mcpctl) for group runs")
    };

    assert!(check("0.5.0-alpha.1"), "a prerelease of 0.5.0 is below 0.5.0");
    assert!(check("v0.4.9+build.7"));
    assert!(!check("0.5.0+build.7"), "build metadata is not a prerelease");
    assert!(!check("0.5.1-rc.1"), "a prerelease of a later version is past 0.5.0");
    assert!(!check("1.0.0"));
    let _ = std::fs::remove_dir_all(&w.home);
}

/// A crc32 for the PNG chunks below.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
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
    png_chunk(&mut out, b"IDAT", &zlib_zeros(height as usize * (1 + 3 * width as usize)));
    png_chunk(&mut out, b"IEND", &[]);
    out
}
