//! A design group's member gate is scoped to what the group has changed so far; the full gate runs once,
//! after the last member, before delivery. Each member gate writes its own log and says its scope.

use std::path::PathBuf;
use std::process::{Command, Output};

use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

const DESIGN_MD: &str = "# Home\n\n### H01 Hero\n\nThe hero.\n";

/// A Django-shaped project (uv.lock, src/manage.py, pytest.ini, apps `shop` and `blog` with tests) with the
/// design group `std,fa`: std is a standard member whose session changes `src/shop`, fa a frontend member
/// (component H01 of the package `designs/home/`) whose session changes `src/blog`. The page never starts, so
/// fa's design review is blocked and the group stops after fa's gate. `uv` is a fake that writes each call to
/// `$HOME/uv-calls`, and fails once when `$HOME/fail-once` exists.
struct Project {
    repo: GitFixture,
    home: PathBuf,
}

/// The fake home of the project `tag`.
fn home_of(tag: &str) -> PathBuf {
    orq_test_support::process_temp_dir().join(format!("orq-member-gate-{}-{tag}", std::process::id()))
}

fn project(tag: &str, run_toml_extra: &str) -> Project {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    let home = home_of(tag);
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
    let write = |path: &str, body: &str| {
        let path = repo.root().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    write(
        ".orq/run.toml",
        &format!(
            "{run_toml_extra}\n[design]\nstart_command = [\"false\"]\nmode = \"infer\"\n\n[design.viewports]\ndesktop = \"400x300\"\ntablet = \"300x400\"\nmobile = \"200x400\"\n"
        ),
    );
    write("designs/home/DESIGN.md", DESIGN_MD);
    for (name, width, height) in [("desktop", 400, 300), ("tablet", 300, 400), ("mobile", 200, 400)] {
        let path = repo.root().join(format!("designs/home/references/{name}.png"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, png(width, height)).unwrap();
    }
    write("uv.lock", "");
    write("pytest.ini", "[pytest]\n");
    write("src/manage.py", "");
    write("src/shop/models.py", "");
    write("src/shop/tests/test_shop.py", "");
    write("src/blog/models.py", "");
    write("src/blog/tests/test_blog.py", "");
    write_ledger(
        repo.root(),
        &[
            LedgerEntry::new("std", "pending"),
            LedgerEntry::new("fa", "pending").description("Design package: home\nDesign components: H01\n"),
        ],
    );
    git_q(&["add", "-A"]);
    git_q(&["commit", "--quiet", "-m", "django project"]);
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
    let p = Project { repo, home };
    p.prepare();
    p
}

impl Project {
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

    /// The project's design reviewer is a pi that is never reached, and fa is a frontend feature.
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
        let kind = self
            .command()
            .args(["feature", "kind", "--repo"])
            .arg(self.repo.root())
            .args(["--id", "fa", "frontend"])
            .output()
            .unwrap();
        assert!(kind.status.success(), "{}", text(&kind.stderr));
    }

    fn orq_run(&self, provider: &orq_test_support::FakeProvider, args: &[&str], env: &[(&str, &str)]) -> Output {
        let agent = orq_test_support::FakeAgent::builder().commit("feat: work").build();
        let bin = self.home.join("bin");
        // std's session changes the shop app, fa's the blog app, a gate fix session the shop app, and any
        // other session (the group's reviews) a file of its own.
        let claude = format!(
            "#!/bin/sh\nprintf '%s\\n----\\n' \"$*\" >> \"$HOME/sessions\"\n\
             case \"$*\" in\n\
             *'implement-and-review std '*) date +%s%N >> src/shop/views.py ;;\n\
             *'implement-and-review fa '*) date +%s%N >> src/blog/views.py ;;\n\
             *'local gate failed'*) date +%s%N >> src/shop/fix.py ;;\n\
             *) date +%s%N >> work.txt ;;\n\
             esac\n\
             exec '{}' \"$@\"\n",
            agent.path().display()
        );
        write_executable(&bin.join("claude"), &claude);
        let pi = "#!/bin/sh\nif [ \"$1\" = --list-models ]; then printf 'provider  model  context  max-out  thinking  images\\nopenai  gpt-6.1-sol  400K  128K  yes  yes\\n'; exit 0; fi\n\
                  for a; do last=$a; done\n\
                  case \"$last\" in *'You are the design preparer'*) ;; *) exit 1 ;; esac\n\
                  cat \"$HOME/preparer.json\"\n";
        write_executable(&bin.join("pi"), pi);
        let uv = "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HOME/uv-calls\"\n\
                  case \"$*\" in *collect-only*) exit 0 ;; esac\n\
                  case \"$*\" in 'run pytest'*) if [ -f \"$HOME/fail-once\" ]; then rm \"$HOME/fail-once\"; echo 'FAILED src/shop/tests/test_shop.py::test_it' ; exit 1; fi ;; esac\n\
                  exit 0\n";
        write_executable(&bin.join("uv"), uv);
        let mut dirs = vec![bin, provider.bin_dir()];
        dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let mut command = self.command();
        command.args(args).env("PATH", std::env::join_paths(dirs).unwrap());
        for (name, value) in env {
            command.env(name, value);
        }
        let out = command.output().unwrap();
        drop(agent);
        out
    }

    /// Runs the group `std,fa` until fa's design review stops it: its run id and output.
    fn run_group(&self, provider: &orq_test_support::FakeProvider, env: &[(&str, &str)]) -> (String, String) {
        let root = self.repo.root().to_str().unwrap().to_string();
        let out = self.orq_run(
            provider,
            &["run", "--repo", &root, "--group", "std,fa", "--remote", "origin", "--target", "main", "--no-ci-check"],
            env,
        );
        let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
        assert!(!out.status.success(), "fa's design review stops the group: {all}");
        (self.run_ids().pop().unwrap_or_else(|| panic!("no run: {all}")), all)
    }

    /// Marks every member of `run_id` passed and the run unended, then resumes it: the resumed run's id.
    fn resume_all_passed(&self, provider: &orq_test_support::FakeProvider, run_id: &str, env: &[(&str, &str)]) -> (String, String) {
        let log = self.home.join("state/orq/runs").join(run_id).join("events.jsonl");
        let lines: Vec<String> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .filter(|l| !l.contains("\"run.completed\""))
            .map(str::to_string)
            .collect();
        let step = lines
            .iter()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find(|e| e["type"] == "design_group.member_step")
            .unwrap();
        let mut kept: String = lines.iter().map(|l| format!("{l}\n")).collect();
        for (index, member) in ["std", "fa"].iter().enumerate() {
            let mut passed = step.clone();
            passed["type"] = "design_group.member_passed".into();
            passed["event_id"] = format!("{}-passed-{member}", step["event_id"].as_str().unwrap()).into();
            passed["payload"] = serde_json::json!({"member": member, "index": index + 1, "of": 2, "step": "passed"});
            kept.push_str(&format!("{passed}\n"));
        }
        std::fs::write(&log, kept).unwrap();
        let root = self.repo.root().to_str().unwrap().to_string();
        let out = self.orq_run(provider, &["run", "resume", run_id, "--yes", "--no-ci-check", "--repo", &root], env);
        let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
        let resumed = self.run_ids().into_iter().find(|id| id != run_id).unwrap_or_else(|| panic!("no resumed run: {all}"));
        (resumed, all)
    }

    fn uv_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.home.join("uv-calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The gate's pytest runs so far, each with its arguments after `pytest`.
    fn pytest_runs(&self) -> Vec<String> {
        self.uv_calls()
            .into_iter()
            .filter(|c| c.starts_with("run pytest") && !c.contains("collect-only"))
            .map(|c| c["run pytest".len()..].trim().to_string())
            .collect()
    }

    fn sessions(&self) -> Vec<String> {
        let text = std::fs::read_to_string(self.home.join("sessions")).unwrap_or_default();
        text.split("\n----\n").filter(|s| !s.trim().is_empty()).map(str::to_string).collect()
    }

    fn events(&self, run_id: &str) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.home.join("state/orq/runs").join(run_id).join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .collect()
    }

    fn logs(&self, run_id: &str) -> PathBuf {
        self.home.join("state/orq/logs").join(run_id)
    }

    /// What a run did, kept beside the build for reading when an assertion fails.
    fn note(&self, test: &str, run_id: &str, all: &str) {
        let events: Vec<String> = self.events(run_id).iter().map(|e| format!("{} {} {}", e["type"], e["stage"], e["payload"])).collect();
        let note = format!("{all}\n== uv\n{:#?}\n== sessions\n{:#?}\n== events\n{}\n", self.uv_calls(), self.sessions(), events.join("\n"));
        let _ = std::fs::write(PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("member-gate-{test}-{run_id}.txt")), note);
    }

    fn run_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = std::fs::read_dir(self.home.join("state/orq/runs"))
            .map(|dir| dir.map(|d| d.unwrap().file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        ids.sort();
        ids
    }
}

fn write_executable(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

/// The payloads of the gate's `stage.started` events.
fn gate_starts(events: &[serde_json::Value]) -> Vec<serde_json::Value> {
    events
        .iter()
        .filter(|e| e["type"] == "stage.started" && e["stage"] == "verify")
        .map(|e| e["payload"].clone())
        .collect()
}

fn provider() -> orq_test_support::FakeProvider {
    orq_test_support::FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], "https://github.example/o/r/pull/7\n")
        .reply(&["pr", "view"], "{\"number\":7,\"url\":\"https://github.example/o/r/pull/7\"}\n")
        .build()
}

#[test]
fn a_django_design_groups_member_gates_run_only_the_changed_apps_tests_and_the_full_gate_runs_once_at_the_end() {
    let p = project("django", "");
    let provider = provider();

    let (run_id, all) = p.run_group(&provider, &[]);
    p.note("django", &run_id, &all);

    assert_eq!(
        p.pytest_runs(),
        ["src/shop", "src/blog src/shop"],
        "after std, shop's tests; after fa, both members' apps: {:#?}\n{all}",
        p.uv_calls()
    );
    assert!(
        p.uv_calls().iter().filter(|c| c.contains("makemigrations --check")).count() == 2,
        "each member gate still checks migrations: {:#?}",
        p.uv_calls()
    );
    let logs = p.logs(&run_id);
    assert!(logs.join("gate-std.log").is_file() && logs.join("gate-fa.log").is_file(), "{all}");
    assert!(!logs.join("gate.log").exists(), "a member gate does not write the final gate's log");
    let starts = gate_starts(&p.events(&run_id));
    assert_eq!(starts.len(), 2, "{starts:#?}");
    assert_eq!((starts[0]["scope"].as_str(), starts[0]["member"].as_str()), (Some("member"), Some("std")));
    assert_eq!(starts[0]["apps"], serde_json::json!(["shop"]));
    assert_eq!((starts[1]["scope"].as_str(), starts[1]["member"].as_str()), (Some("member"), Some("fa")));
    assert_eq!(starts[1]["apps"], serde_json::json!(["blog", "shop"]));

    let (resumed, all) = p.resume_all_passed(&provider, &run_id, &[]);

    p.note("django-resumed", &resumed, &all);
    assert_eq!(
        p.pytest_runs(),
        ["src/shop", "src/blog src/shop", ""],
        "the member gates, then the whole suite exactly once: {all}"
    );
    assert!(p.logs(&resumed).join("gate.log").is_file(), "{all}");
    let starts = gate_starts(&p.events(&resumed));
    assert_eq!(starts.len(), 1, "the final gate starts once: {starts:#?}\n{all}");
    assert!(starts[0]["scope"] == "full" && starts[0].get("member").is_none(), "{starts:#?}");
    let _ = std::fs::remove_dir_all(&p.home);
}

/// A configured gate that writes `word` to `$HOME/gates` of the project `tag`.
fn gate_writing(tag: &str, word: &str) -> String {
    format!("[\"sh\", \"-c\", \"echo {word} >> {}\"]", home_of(tag).join("gates").display())
}

fn gates(p: &Project) -> Vec<String> {
    std::fs::read_to_string(p.home.join("gates")).unwrap_or_default().lines().map(str::to_string).collect()
}

#[test]
fn a_configured_gate_runs_in_full_after_each_member_when_no_member_gate_is_set() {
    let p = project("configured", &format!("gate = {}\n", gate_writing("configured", "full")));
    let provider = provider();

    let (run_id, all) = p.run_group(&provider, &[]);
    p.note("configured", &run_id, &all);

    assert_eq!(gates(&p), ["full", "full"], "{all}");
    assert!(p.pytest_runs().is_empty(), "a configured gate replaces the detected one: {:#?}", p.uv_calls());
    let starts = gate_starts(&p.events(&run_id));
    assert_eq!(starts.len(), 2, "{starts:#?}");
    assert!(starts.iter().all(|s| s["scope"] == "member" && s.get("apps").is_none()), "{starts:#?}");
    let _ = std::fs::remove_dir_all(&p.home);
}

#[test]
fn a_member_gate_runs_after_each_member_and_the_configured_gate_at_the_end() {
    let extra = format!(
        "gate = {}\nmember_gate = {}\n",
        gate_writing("membergate", "full"),
        gate_writing("membergate", "member")
    );
    let p = project("membergate", &extra);
    let provider = provider();

    let (run_id, all) = p.run_group(&provider, &[]);
    p.note("membergate", &run_id, &all);
    assert_eq!(gates(&p), ["member", "member"], "{all}");

    let (_, all) = p.resume_all_passed(&provider, &run_id, &[]);
    assert_eq!(gates(&p).last().map(String::as_str), Some("full"), "{all}");
    assert_eq!(gates(&p).iter().filter(|g| *g == "member").count(), 2, "{all}");
    let _ = std::fs::remove_dir_all(&p.home);
}

#[test]
fn a_failing_member_gate_gets_a_fix_session_reading_the_members_own_log() {
    let p = project("failing", "");
    let provider = provider();
    std::fs::write(p.home.join("fail-once"), "").unwrap();

    let (run_id, all) = p.run_group(&provider, &[]);
    p.note("failing", &run_id, &all);

    let fixes: Vec<String> = p.sessions().into_iter().filter(|s| s.contains("local gate failed")).collect();
    assert_eq!(fixes.len(), 1, "{all}");
    assert!(fixes[0].contains("gate-std.log"), "the fix session reads std's gate log: {}", fixes[0]);
    let logs = p.logs(&run_id);
    assert!(logs.join("gate-fix-std.log").is_file(), "{all}");
    assert!(!logs.join("gate-fix.log").exists() && !logs.join("gate.log").exists(), "{all}");
    assert_eq!(p.pytest_runs(), ["src/shop", "src/shop", "src/blog src/shop"], "the gate runs again after the fix: {all}");
    let _ = std::fs::remove_dir_all(&p.home);
}

#[test]
fn a_single_feature_runs_gate_is_the_full_gate_as_before() {
    let p = project("single", "");
    let provider = provider();
    let root = p.repo.root().to_str().unwrap().to_string();

    let out = p.orq_run(&provider, &["run", "--repo", &root, "--feature", "std", "--remote", "origin", "--target", "main", "--no-ci-check"], &[]);
    let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
    let run_id = p.run_ids().pop().unwrap_or_else(|| panic!("no run: {all}"));
    p.note("single", &run_id, &all);

    assert_eq!(p.pytest_runs(), [""], "{all}");
    let logs = p.logs(&run_id);
    assert!(logs.join("gate.log").is_file() && !logs.join("gate-std.log").exists(), "{all}");
    let starts = gate_starts(&p.events(&run_id));
    assert!(!starts.is_empty() && starts.iter().all(|s| s["scope"] == "full" && s.get("member").is_none()), "{starts:#?}");
    let _ = std::fs::remove_dir_all(&p.home);
}

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

fn zlib_zeros(n: usize) -> Vec<u8> {
    // Stored (uncompressed) deflate blocks of at most 65535 bytes, then the Adler-32 of the zeros.
    let mut out = vec![0x78, 0x01];
    let mut left = n;
    loop {
        let len = left.min(65535);
        left -= len;
        out.push(if left == 0 { 1 } else { 0 });
        out.extend_from_slice(&(len as u16).to_le_bytes());
        out.extend_from_slice(&(!(len as u16)).to_le_bytes());
        out.extend(std::iter::repeat_n(0u8, len));
        if left == 0 {
            break;
        }
    }
    let (a, b) = (0..n).fold((1u32, 0u32), |(a, b), _| (a, (b + a) % 65521));
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

/// A plain grey RGB PNG of `width` x `height`.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &header);
    png_chunk(&mut out, b"IDAT", &zlib_zeros(((width * 3 + 1) * height) as usize));
    png_chunk(&mut out, b"IEND", &[]);
    out
}
