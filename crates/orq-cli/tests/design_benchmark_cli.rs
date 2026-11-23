//! `orq design benchmark --repo <path> --cases <dir> [--agent <harness>/<model>[:effort]]
//! [--repeat N]`: the design reviewer measured on known cases. The reviewer is a fake pi on `PATH`
//! that records its argv and passes every review.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use orq_core::project_settings::{ProjectAgent, ProjectSettings, SaveOutcome};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_test_support::{GitFixture, TempDir};

const PI_MODELS: &str = "provider  model          context  max-out  thinking  images\nopenai    gpt-6.1-sol    400K     128K     yes       yes\nopenai    gpt-6.1-terra  400K     128K     yes       yes\n";

const PASS: &str = r#"{"run_id":"@RUN@","reference_hash":"@REF@","capture_revision":"@CAP@","evaluation":"same","verdict":"pass","checks":[],"issues":[],"suggestions":[],"blockers":[]}"#;

struct Fixture {
    repo: GitFixture,
    home: TempDir,
    cases: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let home = TempDir::new("orq-design-benchmark-cli");
        let bin = home.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let d = home.path().display();
        fs::write(home.path().join("answer.json"), PASS).unwrap();
        fs::write(
            bin.join("pi"),
            format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = --list-models ]; then printf '%s' '{PI_MODELS}'; exit 0; fi\n\
                 for a; do prompt=$a; done\n\
                 n=$(cat '{d}/count' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{d}/count'\n\
                 printf '%s\\n' \"$@\" > '{d}/argv-'$n\n\
                 run=$(printf '%s\\n' \"$prompt\" | sed -n 's/^run_id: //p' | head -n 1)\n\
                 ref=$(printf '%s\\n' \"$prompt\" | sed -n 's/^reference_hash: //p' | head -n 1)\n\
                 cap=$(printf '%s\\n' \"$prompt\" | sed -n 's/^capture_revision: //p' | head -n 1)\n\
                 sed -e \"s|@RUN@|$run|g\" -e \"s|@REF@|$ref|g\" -e \"s|@CAP@|$cap|g\" '{d}/answer.json'\n"
            ),
        )
        .unwrap();
        fs::set_permissions(bin.join("pi"), fs::Permissions::from_mode(0o755)).unwrap();
        let cases = TempDir::new("orq-design-benchmark-cli-cases");
        let control = cases.path().join("control");
        fs::create_dir_all(&control).unwrap();
        fs::write(control.join("reference.png"), b"\x89PNG\r\n\x1a\nsame").unwrap();
        fs::write(control.join("actual.png"), b"\x89PNG\r\n\x1a\nsame").unwrap();
        fs::write(
            control.join("case.json"),
            r#"{"viewport": "desktop", "control": true}"#,
        )
        .unwrap();
        Fixture {
            repo: GitFixture::new(),
            home,
            cases,
        }
    }

    fn roots(&self) -> StorageRoots {
        let base = self.home.path().to_path_buf();
        StorageRoots::resolve(&|name| match name {
            "XDG_DATA_HOME" => Some(base.join("data").display().to_string()),
            "XDG_CONFIG_HOME" => Some(base.join("config").display().to_string()),
            "XDG_STATE_HOME" => Some(base.join("state").display().to_string()),
            "XDG_CACHE_HOME" => Some(base.join("cache").display().to_string()),
            _ => None,
        })
        .unwrap()
    }

    fn set_design_reviewer(&self, model: &str) {
        let roots = self.roots();
        roots.create_all().unwrap();
        let mut store = Store::open(&roots.database()).unwrap();
        let root = fs::canonicalize(self.repo.root()).unwrap();
        let revision = store.project_settings(&root).unwrap().revision;
        let settings = ProjectSettings {
            design_reviewer: Some(ProjectAgent {
                harness: "pi".to_string(),
                model: model.to_string(),
                effort: Some("medium".to_string()),
            }),
            ..ProjectSettings::default()
        };
        assert!(matches!(
            store
                .save_project_settings(&root, revision, &settings)
                .unwrap(),
            SaveOutcome::Saved { .. }
        ));
    }

    fn benchmark(&self, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.home.path().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        orq_test_support::sandboxed_command(env!("CARGO_BIN_EXE_orq"))
            .args(["design", "benchmark", "--repo"])
            .arg(self.repo.root())
            .arg("--cases")
            .arg(self.cases.path())
            .args(args)
            .env("PATH", path)
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("XDG_DATA_HOME", self.home.path().join("data"))
            .env("XDG_STATE_HOME", self.home.path().join("state"))
            .env("XDG_CACHE_HOME", self.home.path().join("cache"))
            .env_remove("HERDR_ACTIVE_PANE_CWD")
            .output()
            .unwrap()
    }

    fn argv(&self, n: u32) -> String {
        fs::read_to_string(self.home.path().join(format!("argv-{n}"))).unwrap_or_default()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Every path under `dir`, relative to it, with its bytes.
fn tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(folder) = pending.pop() {
        let Ok(read) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path.clone());
                out.push((path.strip_prefix(dir).unwrap().to_path_buf(), Vec::new()));
            } else {
                let bytes = fs::read(&path).unwrap_or_default();
                out.push((path.strip_prefix(dir).unwrap().to_path_buf(), bytes));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn without_the_agent_option_the_project_reviewer_is_measured() {
    let f = Fixture::new();
    f.set_design_reviewer("openai/gpt-6.1-terra");

    let output = f.benchmark(&[]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let argv = f.argv(1);
    assert!(
        argv.contains("openai/gpt-6.1-terra\n") && argv.contains("medium\n"),
        "{argv}"
    );
    assert!(f.argv(2).is_empty());
}

#[test]
fn without_a_reviewer_or_with_a_bad_agent_or_repeat_it_is_refused_before_any_review() {
    let f = Fixture::new();
    for args in [
        &[][..],
        &["--agent", "pi"],
        &["--agent", "pi/openai/gpt-6.1-nope"],
        &["--agent", "codex/gpt"],
        &["--agent", "pi/openai/gpt-6.1-sol", "--repeat", "6"],
        &["--agent", "pi/openai/gpt-6.1-sol", "--repeat", "x"],
    ] {
        let output = f.benchmark(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {}", text(&output.stderr));
    }
    assert!(f.argv(1).is_empty());
    assert!(!f.roots().state.join("design-benchmarks").exists());
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn the_agent_option_overrides_the_project_reviewer_and_only_the_benchmark_folder_is_written() {
    let f = Fixture::new();
    f.set_design_reviewer("openai/gpt-6.1-terra");
    let head = git(f.repo.root(), &["rev-parse", "HEAD"]);
    let data = tree(&f.roots().data);
    let cases = tree(f.cases.path());
    let state_before = tree(&f.roots().state);

    let output = f.benchmark(&["--agent", "pi/openai/gpt-6.1-sol:high", "--repeat", "2"]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert!(stdout.contains("pi/openai/gpt-6.1-sol:high"), "{stdout}");
    assert!(stdout.contains("1/1 controls passed"), "{stdout}");
    let report: PathBuf = stdout
        .lines()
        .find_map(|line| line.strip_prefix("report: "))
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("no report path in {stdout}"));
    assert!(report.is_file(), "{}", report.display());
    let benchmarks = f.roots().state.join("design-benchmarks");
    assert!(report.starts_with(&benchmarks), "{}", report.display());
    assert!(report.parent().unwrap().join("results.json").is_file());
    for n in [1, 2] {
        let argv = f.argv(n);
        assert!(argv.contains("openai/gpt-6.1-sol\n") && argv.contains("high\n"), "{argv}");
        assert!(!argv.contains("terra"), "{argv}");
    }
    assert!(f.argv(3).is_empty());
    // Nothing else changed: the repo, orq's data, the cases, and the state outside the benchmark.
    assert_eq!(git(f.repo.root(), &["rev-parse", "HEAD"]), head);
    assert_eq!(git(f.repo.root(), &["status", "--porcelain", "--ignored"]), "");
    assert_eq!(tree(&f.roots().data), data);
    assert_eq!(tree(f.cases.path()), cases);
    let state_after: Vec<_> = tree(&f.roots().state)
        .into_iter()
        .filter(|(path, _)| !path.starts_with("design-benchmarks"))
        .collect();
    assert_eq!(state_after, state_before);
}
