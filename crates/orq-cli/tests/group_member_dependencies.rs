//! A group whose later members depend on earlier ones runs every member: a member a session of the run built is
//! `awaiting_delivery` on the run's branch, which the session's `init_feature` takes as a met dependency, and it
//! stays so until the run delivers.

use std::path::PathBuf;
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

const DESIGN_MD: &str = "# Home\n\n### H01 Hero\n\nThe hero.\n";

/// The design group `a,b,c`: b depends on a, c on b (and on `extra` when given); c is a frontend member (H01 of
/// `designs/home/`) whose page never starts, so its design review is blocked and the group stops after c's gate.
/// Each implement session is a fake claude that first checks, as the TDD server's `init_feature` does, that each
/// of its feature's dependencies is `completed` or `awaiting_delivery` in the ledger (writing `<feature> <dependency> <status>` to
/// `$HOME/seen`, and refusing with `$HOME/refused` when one is not), then marks its feature `completed` and
/// commits `<feature>.txt`.
struct Group {
    repo: GitFixture,
    home: PathBuf,
}

fn group(tag: &str, b_depends_on: &[&str], extra: Vec<LedgerEntry>, run_toml: &str) -> Group {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    let home = orq_test_support::process_temp_dir().join(format!("orq-group-deps-{}-{tag}", std::process::id()));
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
    let mut entries = vec![
        LedgerEntry::new("a", "pending"),
        LedgerEntry::new("b", "pending").depends_on(b_depends_on),
        LedgerEntry::new("c", "pending").depends_on(&["b"]).description("Design package: home\nDesign components: H01\n"),
    ];
    entries.extend(extra);
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
    g.prepare();
    g
}

/// The status of `$1` in the ledger `$2`, as an awk program over its one-member-per-line layout.
const STATUS_OF: &str = "awk -v n=\"$1\" '/\"featureName\":/ { cur = index($0, \"\\\"featureName\\\": \\\"\" n \"\\\"\") > 0 } /\"status\":/ && cur { sub(/.*\"status\": \"/, \"\"); sub(/\".*/, \"\"); print; exit }' \"$2\"";

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

    /// The project's design reviewer is a pi that is never reached, and c is a frontend feature.
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
        store.save_project_settings(&root, revision, &settings).unwrap();
        drop(store);
        let kind = self.command().args(["feature", "kind", "--repo"]).arg(self.repo.root()).args(["--id", "c", "frontend"]).output().unwrap();
        assert!(kind.status.success(), "{}", text(&kind.stderr));
    }

    /// `orq <args>` with the fake claude (dependency check, then `agent`) and a pi that only prepares the package.
    fn orq(&self, agent: &FakeAgent, provider: &FakeProvider, args: &[&str]) -> Output {
        let bin = self.home.join("bin");
        let ledger = self.repo.root().join(".tdd-features.json");
        let claude = format!(
            "#!/bin/sh\nstatus_of() {{ {STATUS_OF}; }}\nset_status() {{ {SET_STATUS}; }}\n\
             ledger='{ledger}'\n\
             printf '%s\\n----\\n' \"$*\" >> \"$HOME/sessions\"\n\
             date +%s%N >> work.txt\n\
             case \"$*\" in\n\
               *'/implement-and-review a '*) f=a ;;\n\
               *'/implement-and-review b '*) f=b ;;\n\
               *'/implement-and-review c '*) f=c ;;\n\
               *) exec '{agent}' \"$@\" ;;\n\
             esac\n\
             deps=$(awk -v n=\"$f\" '/\"dependsOn\": \\[\\]/ {{ list = \"\"; next }} /\"dependsOn\":/ {{ on = 1; list = \"\"; next }} on && /\\]/ {{ on = 0; next }} on {{ gsub(/[\" ,]/, \"\"); list = list \" \" $0; next }} /\"featureName\":/ && index($0, \"\\\"featureName\\\": \\\"\" n \"\\\"\") > 0 {{ print list; exit }}' \"$ledger\")\n\
             for d in $deps; do\n\
               s=$(status_of \"$d\" \"$ledger\")\n\
               echo \"$f $d $s\" >> \"$HOME/seen\"\n\
               if [ \"$s\" != completed ] && [ \"$s\" != awaiting_delivery ]; then echo \"Feature '$f' depends on feature(s) not yet completed: $d\" >> \"$HOME/refused\"; exit 3; fi\n\
             done\n\
             if [ \"$f\" = a ] && [ -f \"$HOME/reopen-x\" ]; then set_status x \"$ledger\" pending; fi\n\
             if [ \"$f\" = a ] && [ -f \"$HOME/corrupt-handoff\" ]; then for r in \"$HOME\"/state/orq/runs/*/; do echo 'not json' > \"$r/handoff.json\"; done; fi\n\
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

    fn run_group(&self, agent: &FakeAgent, provider: &FakeProvider) -> Output {
        let root = self.repo.root().to_str().unwrap().to_string();
        self.orq(agent, provider, &["run", "--repo", &root, "--group", "a,b,c", "--remote", "origin", "--target", "main", "--no-ci-check"])
    }

    fn resume(&self, run_id: &str, agent: &FakeAgent, provider: &FakeProvider) -> Output {
        let root = self.repo.root().to_str().unwrap().to_string();
        self.orq(agent, provider, &["run", "resume", run_id, "--yes", "--no-ci-check", "--repo", &root])
    }

    /// What `$HOME/<name>` holds, one entry per line.
    fn lines_of(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.home.join(name)).unwrap_or_default().lines().map(str::to_string).collect()
    }

    /// The status the project ledger records for `name`.
    fn status_of(&self, name: &str) -> String {
        let ledger: serde_json::Value = serde_json::from_slice(&std::fs::read(self.repo.root().join(".tdd-features.json")).unwrap()).unwrap();
        let entry = ledger.as_array().unwrap().iter().find(|e| e["featureName"] == name).unwrap();
        entry["status"].as_str().unwrap().to_string()
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
        let mut ids: Vec<String> = std::fs::read_dir(self.home.join("state/orq/runs")).unwrap().map(|d| d.unwrap().file_name().to_string_lossy().into_owned()).collect();
        ids.sort();
        ids
    }

    /// Marks c passed and the run `run_id` unended, as if c had got through its design loop before orq died.
    fn pass_c(&self, run_id: &str) {
        let log = self.home.join("state/orq/runs").join(run_id).join("events.jsonl");
        let lines: Vec<String> = std::fs::read_to_string(&log).unwrap().lines().filter(|l| !l.contains("\"run.completed\"")).map(str::to_string).collect();
        let step = lines.iter().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()).find(|e| e["type"] == "design_group.member_step").unwrap();
        let mut kept: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let mut passed = step.clone();
        passed["type"] = "design_group.member_passed".into();
        passed["event_id"] = format!("{}-passed-c", step["event_id"].as_str().unwrap()).into();
        passed["payload"] = serde_json::json!({"member": "c", "index": 3, "of": 3, "step": "passed"});
        kept.push_str(&format!("{passed}\n"));
        std::fs::write(&log, kept).unwrap();
    }
}

/// The payloads of `kind` among `events`, in order.
fn payloads(events: &[serde_json::Value], kind: &str) -> Vec<serde_json::Value> {
    events.iter().filter(|e| e["type"] == kind).map(|e| e["payload"].clone()).collect()
}

/// The kinds of `events` that are member steps (as `step <member> <step>`) or ledger events, in order.
fn ledger_and_steps(events: &[serde_json::Value]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e["type"].as_str().unwrap() {
            "design_group.member_step" => Some(format!("step {} {}", e["payload"]["member"].as_str().unwrap(), e["payload"]["step"].as_str().unwrap())),
            kind if kind.starts_with("ledger.awaiting") => Some(format!("{kind} {}", e["payload"]["features"])),
            _ => None,
        })
        .collect()
}

fn delivering() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], "https://github.example/o/r/pull/7\n")
        .reply(&["pr", "view"], "{\"number\":7,\"url\":\"https://github.example/o/r/pull/7\"}\n")
        .build()
}

#[test]
fn a_chain_of_three_members_runs_each_member_with_its_dependencies_completed_and_delivery_completes_all_three() {
    let w = group("chain", &["a"], Vec::new(), "");
    let agent = FakeAgent::builder().commit("feat: a member").build();
    let provider = delivering();

    let first = w.run_group(&agent, &provider);

    let all = format!("{}{}", text(&first.stdout), text(&first.stderr));
    assert!(all.contains("member 3 of 3: c: the design review is blocked"), "every member ran, c stops at its design review: {all}");
    assert!(w.lines_of("refused").is_empty(), "no session was refused: {:?}\n{all}", w.lines_of("refused"));
    assert_eq!(w.lines_of("seen"), vec!["b a awaiting_delivery", "c b awaiting_delivery"], "{all}");
    for name in ["a", "b", "c"] {
        assert_eq!(w.status_of(name), "awaiting_delivery", "{name} awaits delivery: {all}");
    }
    let run_id = w.run_ids().pop().unwrap();
    let events = w.events(&run_id);
    assert!(payloads(&events, "ledger.released_for_member").is_empty() && payloads(&events, "ledger.held_until_delivery").is_empty());
    let order = ledger_and_steps(&events);
    let awaited_during = |name: &str, from: &str, to: &str| {
        let at = |line: &str| order.iter().position(|l| l == line).unwrap_or_else(|| panic!("{line} missing: {order:#?}"));
        order[at(from)..at(to)].iter().any(|l| l.starts_with("ledger.awaiting_delivery") && l.contains(&format!("\"{name}\"")))
    };
    assert!(awaited_during("a", "step a implement", "step b implement"), "a awaits delivery after its session: {order:#?}");
    assert!(awaited_during("b", "step b implement", "step c implement"), "and b after its: {order:#?}");

    w.pass_c(&run_id);
    let resumed = w.resume(&run_id, &agent, &provider);

    let all = format!("{}{}", text(&resumed.stdout), text(&resumed.stderr));
    assert!(resumed.status.success(), "{all}");
    for name in ["a", "b", "c"] {
        assert_eq!(w.status_of(name), "completed", "delivery completes {name}: {all}");
    }
    let _ = std::fs::remove_dir_all(&w.home);
}

#[test]
fn a_run_failing_at_member_2s_gate_leaves_a_and_b_awaiting_delivery_and_its_resume_goes_on_to_member_3() {
    // b's gate fails while `$HOME/fail-gate` exists; a's and c's pass.
    let w = group("gate", &["a"], Vec::new(), "member_gate = [\"sh\", \"-c\", \"test ! -f b.txt || test ! -f '{home}/fail-gate'\"]\n");
    std::fs::write(w.home.join("fail-gate"), "").unwrap();
    let agent = FakeAgent::builder().commit("feat: a member").build();
    let provider = delivering();

    let first = w.run_group(&agent, &provider);

    let all = format!("{}{}", text(&first.stdout), text(&first.stderr));
    assert!(all.contains("member 2 of 3: b") && all.contains("state: failed"), "the run fails at b's gate: {all}");
    assert_eq!(w.status_of("a"), "awaiting_delivery", "{all}");
    assert_eq!(w.status_of("b"), "awaiting_delivery", "{all}");
    let run_id = w.run_ids().pop().unwrap();

    std::fs::remove_file(w.home.join("fail-gate")).unwrap();
    let resumed = w.resume(&run_id, &agent, &provider);

    let all = format!("{}{}", text(&resumed.stdout), text(&resumed.stderr));
    assert!(all.contains("member 3 of 3: c: the design review is blocked"), "the resume goes on to c: {all}");
    assert!(w.lines_of("refused").is_empty(), "{:?}\n{all}", w.lines_of("refused"));
    assert_eq!(w.lines_of("seen"), vec!["b a awaiting_delivery", "c b awaiting_delivery"], "{all}");
    for name in ["a", "b", "c"] {
        assert_eq!(w.status_of(name), "awaiting_delivery", "{name} awaits delivery: {all}");
    }
    let _ = std::fs::remove_dir_all(&w.home);
}

#[test]
fn a_member_whose_dependency_outside_the_group_is_not_completed_is_still_refused_and_the_earlier_members_still_await_delivery() {
    // b depends on a and on x, outside the group: completed when the run starts, set back to pending by a's
    // session, as another hand might.
    let w = group("outside", &["a", "x"], vec![LedgerEntry::new("x", "completed")], "");
    std::fs::write(w.home.join("reopen-x"), "").unwrap();
    let agent = FakeAgent::builder().commit("feat: a member").build();

    let first = w.run_group(&agent, &delivering());

    let all = format!("{}{}", text(&first.stdout), text(&first.stderr));
    assert!(all.contains("state: failed"), "{all}");
    assert_eq!(w.lines_of("seen"), vec!["b a awaiting_delivery", "b x pending"], "{all}");
    assert_eq!(w.lines_of("refused"), vec!["Feature 'b' depends on feature(s) not yet completed: x"], "{all}");
    assert_eq!(w.status_of("x"), "pending", "a feature outside the run is never touched: {all}");
    assert_eq!(w.status_of("a"), "awaiting_delivery", "a still awaits delivery after b's refused session: {all}");
    let _ = std::fs::remove_dir_all(&w.home);
}

#[test]
fn a_member_session_that_cannot_be_prepared_leaves_the_earlier_members_awaiting_delivery() {
    // a's session leaves the run's handoff unreadable, so b's session cannot be prepared.
    let w = group("unprepared", &["a"], Vec::new(), "");
    std::fs::write(w.home.join("corrupt-handoff"), "").unwrap();
    let agent = FakeAgent::builder().commit("feat: a member").build();

    let first = w.run_group(&agent, &delivering());

    let all = format!("{}{}", text(&first.stdout), text(&first.stderr));
    assert!(all.contains("handoff.json"), "b's session fails on the handoff: {all}");
    assert!(w.lines_of("seen").is_empty(), "b's session never started: {all}");
    assert_eq!(w.status_of("a"), "awaiting_delivery", "a is not left completed: {all}");
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
