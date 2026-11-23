//! A run of a frontend feature uses the frontend steps; a grouped run with a frontend feature is refused for
//! now, with the reason, and every other run starts as today.

use std::path::Path;
use std::process::{Command, Output};

use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

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

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success());
}

fn check_group(repo: &GitFixture, group: &str) -> Output {
    orq(repo)
        .args(["run", "--repo"])
        .arg(repo.root())
        .args(["--remote", "origin", "--target", "main", "--group", group, "--check"])
        .output()
        .unwrap()
}

#[test]
fn a_grouped_run_with_a_frontend_feature_is_refused_with_the_reason() {
    let repo = GitFixture::new();
    git(repo.root(), &["remote", "add", "origin", "git@github.com:o/r.git"]);
    write_ledger(repo.root(), &[LedgerEntry::new("page", "pending"), LedgerEntry::new("engine", "pending")]);

    let standard = check_group(&repo, "page,engine");
    assert!(standard.status.success(), "a group of standard features starts as today: {}", text(&standard.stderr));

    let kind = orq(&repo).args(["feature", "kind", "--repo"]).arg(repo.root()).args(["--id", "page", "frontend"]).output().unwrap();
    assert!(kind.status.success(), "{}", text(&kind.stderr));

    let refused = check_group(&repo, "page,engine");
    assert!(!refused.status.success(), "{}", text(&refused.stdout));
    let stderr = text(&refused.stderr);
    assert!(stderr.contains("page is a frontend feature"), "{stderr}");
    assert!(stderr.contains("cannot run in a group"), "{stderr}");
}

/// A run of feature `home`, marked frontend, whose page never starts, so its design review is blocked before any
/// reviewer: the agents and the forge are fakes, and no browser is needed.
struct Frontend {
    repo: GitFixture,
    home: std::path::PathBuf,
}

fn frontend(tag: &str) -> Frontend {
    frontend_with(tag, true)
}

/// As [`frontend`], with the package's three references only when `references`.
fn frontend_with(tag: &str, references: bool) -> Frontend {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    let home = orq_test_support::process_temp_dir().join(format!("orq-frontend-run-{}-{tag}", std::process::id()));
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
    std::fs::write(repo.root().join("designs/home/DESIGN.md"), "# Home\n").unwrap();
    if references {
        for (name, width, height) in [("desktop", 400, 300), ("tablet", 300, 400), ("mobile", 200, 400)] {
            std::fs::write(repo.root().join(format!("designs/home/references/{name}.png")), png(width, height)).unwrap();
        }
    }
    write_ledger(repo.root(), &[LedgerEntry::new("home", "pending")]);
    git_q(&["add", "-A"]);
    git_q(&["commit", "--quiet", "-m", "design package"]);
    git_q(&["branch", "-M", "main"]);
    git_q(&["push", "--quiet", "origin", "main"]);
    git_q(&["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let ssh = home.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    Frontend { repo, home }
}

impl Frontend {
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

    fn roots(&self) -> orq_core::storage::StorageRoots {
        let base = self.home.clone();
        orq_core::storage::StorageRoots::resolve(&|name| match name {
            "XDG_DATA_HOME" => Some(base.join("data").display().to_string()),
            "XDG_CONFIG_HOME" => Some(base.join("config").display().to_string()),
            "XDG_STATE_HOME" => Some(base.join("state").display().to_string()),
            "XDG_CACHE_HOME" => Some(base.join("cache").display().to_string()),
            _ => None,
        })
        .unwrap()
    }

    /// The project's design reviewer is a pi that is never reached.
    fn design_reviewer(&self) {
        let roots = self.roots();
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
    }

    fn mark_frontend(&self) {
        let kind = self.command().args(["feature", "kind", "--repo"]).arg(self.repo.root()).args(["--id", "home", "frontend"]).output().unwrap();
        assert!(kind.status.success(), "{}", text(&kind.stderr));
    }

    fn run(&self, agent: &orq_test_support::FakeAgent, provider: &orq_test_support::FakeProvider) -> Output {
        self.orq_run(agent, provider, &["run", "--repo", self.repo.root().to_str().unwrap(), "--feature", "home", "--remote", "origin", "--target", "main", "--no-ci-check"])
    }

    fn resume(&self, run_id: &str, agent: &orq_test_support::FakeAgent, provider: &orq_test_support::FakeProvider) -> Output {
        self.orq_run(agent, provider, &["run", "resume", run_id, "--yes", "--no-ci-check", "--repo", self.repo.root().to_str().unwrap()])
    }

    fn orq_run(&self, agent: &orq_test_support::FakeAgent, provider: &orq_test_support::FakeProvider, args: &[&str]) -> Output {
        let bin = self.home.join("bin");
        std::fs::write(bin.join("claude"), format!("#!/bin/sh\nexec '{}' \"$@\"\n", agent.path().display())).unwrap();
        std::fs::set_permissions(bin.join("claude"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        // As the design preparer it answers `$HOME/preparer.json`, or the documents the package
        // lacks with no conflicts and nothing needed; as anything else it fails.
        let pi = "#!/bin/sh\nif [ \"$1\" = --list-models ]; then printf 'provider  model  context  max-out  thinking  images\\nopenai  gpt-6.1-sol  400K  128K  yes  yes\\n'; exit 0; fi\n\
                  for a; do last=$a; done\n\
                  case \"$last\" in *'You are the design preparer'*) ;; *) exit 1 ;; esac\n\
                  printf '%s\\n' \"$last\" > \"$HOME/preparer-prompt\"\n\
                  if [ -f \"$HOME/preparer.json\" ]; then cat \"$HOME/preparer.json\"; else printf '{\"files\":[{\"path\":\"DESIGN.md\",\"content\":\"# Home, prepared\"},{\"path\":\"RESPONSIVE.md\",\"content\":\"Stack.\"},{\"path\":\"tokens-mapping.md\",\"content\":\"none\"}],\"conflicts\":[],\"inferred\":[],\"needed_files\":[]}'; fi\n";
        std::fs::write(bin.join("pi"), pi).unwrap();
        std::fs::set_permissions(bin.join("pi"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let mut dirs = vec![bin, provider.bin_dir()];
        dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        self.command()
            .args(args)
            .env("PATH", std::env::join_paths(dirs).unwrap())
            .output()
            .unwrap()
    }

    /// Every event of the run, as JSON.
    fn events(&self) -> Vec<serde_json::Value> {
        let runs = self.home.join("state/orq/runs");
        let mut events = Vec::new();
        for dir in std::fs::read_dir(&runs).unwrap() {
            let text = std::fs::read_to_string(dir.unwrap().path().join("events.jsonl")).unwrap_or_default();
            events.extend(text.lines().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()));
        }
        events
    }
}

#[test]
fn a_frontend_run_whose_design_review_is_blocked_pauses_for_a_person_and_delivers_nothing() {
    let w = frontend("blocked");
    w.design_reviewer();
    w.mark_frontend();
    let agent = orq_test_support::FakeAgent::builder().write_file("index.html", "<h1>Home</h1>\n").commit("feat: the home page").build();
    let provider = orq_test_support::FakeProvider::builder().reply(&["pr", "list"], "[]\n").build();

    let out = w.run(&agent, &provider);

    let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(!out.status.success(), "{all}");
    assert!(all.contains("needs_review") || all.contains("needs review"), "{all}");
    assert!(all.contains("design review is blocked"), "{all}");
    let events = w.events();
    let started = events
        .iter()
        .find(|e| e["type"] == "stage.started" && e["stage"] == "design_review")
        .unwrap_or_else(|| panic!("no design_review step in {events:?}"));
    assert_eq!(started["payload"], serde_json::json!({"round": 1, "of": 4}), "{started}");
    assert!(!events.iter().any(|e| e["stage"] == "push"), "a failed design review never delivers: {events:?}");
    let _ = std::fs::remove_dir_all(&w.home);
}

#[test]
fn a_resumed_frontend_run_keeps_its_design_rounds_and_its_frozen_package() {
    let w = frontend("resume-rounds");
    w.design_reviewer();
    w.mark_frontend();
    let agent = orq_test_support::FakeAgent::builder().write_file("index.html", "<h1>Home</h1>\n").commit("feat: the home page").build();
    let provider = orq_test_support::FakeProvider::builder().reply(&["pr", "list"], "[]\n").build();
    let first = w.run(&agent, &provider);
    assert!(!first.status.success());
    let first_run = w.events().iter().find_map(|e| e["run_id"].as_str().map(str::to_string)).unwrap();
    // orq dies during the design step after round 1 is recorded: the run has no ending, its owner is gone.
    let log = w.home.join("state/orq/runs").join(&first_run).join("events.jsonl");
    let kept: Vec<String> = std::fs::read_to_string(&log).unwrap().lines().filter(|l| !l.contains("\"run.completed\"")).map(|l| format!("{l}\n")).collect();
    std::fs::write(&log, kept.concat()).unwrap();

    let resumed = w.resume(&first_run, &agent, &provider);

    let all = format!("{}{}", text(&resumed.stdout), text(&resumed.stderr));
    let events = w.events();
    let rounds: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["run_id"] != first_run.as_str() && e["type"] == "stage.started" && e["stage"] == "design_review")
        .map(|e| &e["payload"])
        .collect();
    assert_eq!(rounds, [&serde_json::json!({"round": 2, "of": 4})], "the resumed run goes on from round 2, not round 1: {all}");
    let resumed_run = events.iter().filter_map(|e| e["run_id"].as_str()).find(|r| *r != first_run).unwrap().to_string();
    let manifest = w.home.join("state/orq/logs").join(&resumed_run).join("design/manifest.json");
    assert!(manifest.is_file(), "the frozen package's manifest is carried to the resumed run: {all}");
    let _ = std::fs::remove_dir_all(&w.home);
}

#[test]
fn a_resume_whose_frozen_package_is_gone_is_refused_instead_of_freezing_the_changed_references() {
    let w = frontend("resume-package-gone");
    w.design_reviewer();
    w.mark_frontend();
    let agent = orq_test_support::FakeAgent::builder().write_file("index.html", "<h1>Home</h1>\n").commit("feat: the home page").build();
    let provider = orq_test_support::FakeProvider::builder().reply(&["pr", "list"], "[]\n").build();
    assert!(!w.run(&agent, &provider).status.success());
    let first_run = w.events().iter().find_map(|e| e["run_id"].as_str().map(str::to_string)).unwrap();
    let log = w.home.join("state/orq/runs").join(&first_run).join("events.jsonl");
    let kept: Vec<String> = std::fs::read_to_string(&log).unwrap().lines().filter(|l| !l.contains("\"run.completed\"")).map(|l| format!("{l}\n")).collect();
    std::fs::write(&log, kept.concat()).unwrap();
    // The frozen package is lost and the references change.
    std::fs::remove_dir_all(w.home.join("state/orq/logs").join(&first_run).join("design")).unwrap();
    std::fs::write(w.repo.root().join("designs/home/references/desktop.png"), png(400, 301)).unwrap();

    let resumed = w.resume(&first_run, &agent, &provider);

    let all = format!("{}{}", text(&resumed.stdout), text(&resumed.stderr));
    assert!(!resumed.status.success(), "{all}");
    assert!(all.contains("frozen design package"), "{all}");
    let events = w.events();
    assert!(
        !events.iter().any(|e| e["run_id"] != first_run.as_str() && e["type"] == "stage.started" && e["stage"] == "design_review"),
        "no review against the changed references: {all}"
    );
    let _ = std::fs::remove_dir_all(&w.home);
}

#[test]
fn a_frontend_run_missing_design_files_pauses_as_design_files_needed_before_the_implementer_with_the_list() {
    let w = frontend_with("files-needed", false);
    w.design_reviewer();
    w.mark_frontend();
    let agent = orq_test_support::FakeAgent::builder().write_file("index.html", "<h1>Home</h1>\n").commit("feat: the home page").build();
    let provider = orq_test_support::FakeProvider::builder().reply(&["pr", "list"], "[]\n").build();

    let out = w.run(&agent, &provider);

    let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(all.contains("state: design_files_needed"), "{all}");
    let events = w.events();
    let needed = events
        .iter()
        .find(|e| e["type"] == "design.files_needed")
        .unwrap_or_else(|| panic!("no design.files_needed event in {events:?}"));
    let paths: Vec<&str> = needed["payload"]["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert!(paths.contains(&"designs/home/references/desktop.png"), "{needed}");
    assert!(events.iter().any(|e| e["type"] == "stage.started" && e["stage"] == "design_package"), "{events:?}");
    assert!(!events.iter().any(|e| e["stage"] == "implement_and_review"), "no agent runs before the files are there: {events:?}");
    let _ = std::fs::remove_dir_all(&w.home);
}

#[test]
fn a_frontend_run_waiting_for_design_files_resumes_once_they_are_there_and_goes_on_to_the_implementer() {
    let w = frontend_with("files-supplied", false);
    w.design_reviewer();
    w.mark_frontend();
    let agent = orq_test_support::FakeAgent::builder().write_file("index.html", "<h1>Home</h1>\n").commit("feat: the home page").build();
    let provider = orq_test_support::FakeProvider::builder().reply(&["pr", "list"], "[]\n").build();
    let first = w.run(&agent, &provider);
    assert!(text(&first.stdout).contains("state: design_files_needed"), "{}", text(&first.stdout));
    let first_run = w.events().iter().find_map(|e| e["run_id"].as_str().map(str::to_string)).unwrap();
    for (name, width, height) in [("desktop", 400, 300), ("tablet", 300, 400), ("mobile", 200, 400)] {
        std::fs::write(w.repo.root().join(format!("designs/home/references/{name}.png")), png(width, height)).unwrap();
    }

    let resumed = w.resume(&first_run, &agent, &provider);

    let all = format!("{}{}", text(&resumed.stdout), text(&resumed.stderr));
    let events = w.events();
    let later = |e: &&serde_json::Value| e["run_id"] != first_run.as_str();
    assert!(
        events.iter().filter(later).any(|e| e["type"] == "stage.completed" && e["stage"] == "implement_and_review"),
        "the implementer runs once the files are there: {all}"
    );
    let started = events
        .iter()
        .filter(later)
        .find(|e| e["type"] == "stage.started" && e["stage"] == "design_review")
        .unwrap_or_else(|| panic!("no design review after the resume: {all}"));
    assert_eq!(started["payload"], serde_json::json!({"round": 1, "of": 4}), "{all}");
    let _ = std::fs::remove_dir_all(&w.home);
}

#[test]
fn an_infer_mode_run_pauses_for_an_asset_its_preparer_needs_and_resumed_with_it_prepares_again_and_implements() {
    let w = frontend("infer-asset");
    w.design_reviewer();
    w.mark_frontend();
    std::fs::write(
        w.home.join("preparer.json"),
        r#"{"conflicts":[],"inferred":[],"needed_files":[{"path":"assets/hero.png","what":"the hero illustration","why":"the desktop reference shows it"}]}"#,
    )
    .unwrap();
    let agent = orq_test_support::FakeAgent::builder().write_file("index.html", "<h1>Home</h1>\n").commit("feat: the home page").build();
    let provider = orq_test_support::FakeProvider::builder().reply(&["pr", "list"], "[]\n").build();

    let first = w.run(&agent, &provider);

    let all = format!("{}{}", text(&first.stdout), text(&first.stderr));
    assert!(all.contains("state: design_files_needed"), "{all}");
    assert!(std::fs::read_to_string(w.home.join("preparer-prompt")).unwrap().contains("design-prepare"), "the preparer is given its skill");
    let events = w.events();
    let needed = events
        .iter()
        .find(|e| e["type"] == "design.files_needed")
        .unwrap_or_else(|| panic!("no design.files_needed event in {events:?}"));
    let files = needed["payload"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "{needed}");
    assert_eq!(files[0]["path"], "designs/home/assets/hero.png", "{needed}");
    assert!(files[0]["what"].as_str().unwrap().contains("hero illustration"), "{needed}");
    assert!(!events.iter().any(|e| e["stage"] == "implement_and_review"), "no implementer before the asset is there: {events:?}");
    let first_run = events.iter().find_map(|e| e["run_id"].as_str().map(str::to_string)).unwrap();

    std::fs::create_dir_all(w.repo.root().join("designs/home/assets")).unwrap();
    std::fs::write(w.repo.root().join("designs/home/assets/hero.png"), png(40, 30)).unwrap();
    std::fs::remove_file(w.home.join("preparer.json")).unwrap();
    let resumed = w.resume(&first_run, &agent, &provider);

    let all = format!("{}{}", text(&resumed.stdout), text(&resumed.stderr));
    let events = w.events();
    let later = |e: &&serde_json::Value| e["run_id"] != first_run.as_str();
    assert!(
        events.iter().filter(later).any(|e| e["type"] == "stage.completed" && e["stage"] == "implement_and_review"),
        "the implementer runs once the asset is there: {all}"
    );
    let resumed_run = events.iter().filter_map(|e| e["run_id"].as_str()).find(|r| *r != first_run).unwrap().to_string();
    let logs = w.home.join("state/orq/logs").join(&resumed_run).join("design");
    let manifest = std::fs::read_to_string(logs.join("manifest.json")).unwrap_or_else(|e| panic!("{e}: {all}"));
    assert!(manifest.contains("assets/hero.png"), "the changed package is frozen again: {manifest}");
    let prepared = std::fs::read_to_string(logs.join("prepared.json")).unwrap_or_else(|e| panic!("{e}: {all}"));
    assert!(prepared.contains("DESIGN.md"), "prepared again: {prepared}");
    let _ = std::fs::remove_dir_all(&w.home);
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
