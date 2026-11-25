//! A run with a frontend feature is refused before anything is recorded when the project cannot run its design
//! steps: no `[design]` table in `.orq/run.toml`, or design agents the project settings do not accept. A run with
//! no frontend feature is not checked.

use std::path::PathBuf;
use std::process::{Command, Output};

use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

const EXIT_INELIGIBLE: i32 = 20;

const DESIGN_TABLE: &str = "[design]\nstart_command = [\"false\"]\nmode = \"infer\"\n\n[design.viewports]\ndesktop = \"400x300\"\ntablet = \"300x400\"\nmobile = \"200x400\"\n";

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A project with pending features `std` and `page`, its own orq folders and a `bin` folder put first on `PATH`.
struct Project {
    repo: GitFixture,
    home: PathBuf,
}

fn project(tag: &str, design_table: bool) -> Project {
    let repo = GitFixture::new();
    let git = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(repo.root()).args(args).status().unwrap().success());
    git(&["remote", "add", "origin", "git@github.com:o/r.git"]);
    write_ledger(repo.root(), &[LedgerEntry::new("std", "pending"), LedgerEntry::new("page", "pending")]);
    if design_table {
        std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
        std::fs::write(repo.root().join(".orq/run.toml"), DESIGN_TABLE).unwrap();
    }
    let home = orq_test_support::process_temp_dir().join(format!("orq-refuses-frontend-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("bin")).unwrap();
    Project { repo, home }
}

impl Project {
    fn command(&self) -> Command {
        let mut dirs = vec![self.home.join("bin")];
        dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let mut command = orq_test_support::sandboxed_command(env!("CARGO_BIN_EXE_orq"));
        command
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("XDG_STATE_HOME", self.home.join("state"))
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .env("PATH", std::env::join_paths(dirs).unwrap())
            .env_remove("HERDR_ACTIVE_PANE_CWD")
            .env_remove("TDD_FEATURES_PATH");
        command
    }

    fn mark_frontend(&self, feature: &str) {
        let kind = self.command().args(["feature", "kind", "--repo"]).arg(self.repo.root()).args(["--id", feature, "frontend"]).output().unwrap();
        assert!(kind.status.success(), "{}", text(&kind.stderr));
    }

    /// The project's design reviewer is pi with `model`.
    fn design_reviewer(&self, model: &str) {
        let base = self.home.clone();
        let roots = orq_core::storage::StorageRoots::resolve(&|name| match name {
            "XDG_DATA_HOME" => Some(base.join("data").display().to_string()),
            "XDG_CONFIG_HOME" => Some(base.join("config").display().to_string()),
            "XDG_STATE_HOME" => Some(base.join("state").display().to_string()),
            "XDG_CACHE_HOME" => Some(base.join("cache").display().to_string()),
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
                model: model.to_string(),
                effort: Some("medium".to_string()),
            }),
            ..Default::default()
        };
        store.save_project_settings(&root, revision, &settings).unwrap();
    }

    /// A fake `pi` whose `--list-models` lists `openai/gpt-6.1-sol` with images and `openai/text-only` without.
    fn fake_pi(&self) {
        self.pi_script("#!/bin/sh\nif [ \"$1\" = --list-models ]; then printf 'provider  model  context  max-out  thinking  images\\nopenai  gpt-6.1-sol  400K  128K  yes  yes\\nopenai  text-only  400K  128K  yes  no\\n'; exit 0; fi\nexit 1\n");
    }

    /// A fake `pi` that is `script`.
    fn pi_script(&self, script: &str) {
        let pi = self.home.join("bin/pi");
        std::fs::write(&pi, script).unwrap();
        std::fs::set_permissions(&pi, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }

    fn run(&self, extra: &[&str]) -> Output {
        self.command().args(["run", "--repo"]).arg(self.repo.root()).args(["--remote", "origin", "--target", "main"]).args(extra).output().unwrap()
    }

    /// The runs recorded so far.
    fn runs(&self) -> usize {
        std::fs::read_dir(self.home.join("state/orq/runs")).map(|dir| dir.count()).unwrap_or(0)
    }

    fn worktrees(&self) -> usize {
        let out = Command::new("git").arg("-C").arg(self.repo.root()).args(["worktree", "list", "--porcelain"]).output().unwrap();
        text(&out.stdout).lines().filter(|l| l.starts_with("worktree ")).count()
    }
}

fn assert_refused(project: &Project, output: &Output, feature: &str, reason: &str) {
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(EXIT_INELIGIBLE), "{stderr}");
    assert!(stderr.contains(reason), "{stderr}");
    assert!(stderr.contains(&format!("feature {feature} ")), "names {feature}: {stderr}");
    assert_eq!(project.runs(), 0, "no run is recorded: {stderr}");
    assert_eq!(project.worktrees(), 1, "no worktree is made: {stderr}");
}

#[test]
fn a_frontend_run_without_a_design_reviewer_is_refused_before_it_is_recorded() {
    let p = project("no-reviewer", true);
    p.mark_frontend("page");
    for extra in [&["--feature", "page", "--check"][..], &["--feature", "page", "--no-ci-check"][..]] {
        let output = p.run(extra);
        assert_refused(&p, &output, "page", "set a design reviewer in Settings");
    }
}

#[test]
fn a_group_with_a_frontend_member_is_refused_naming_that_member() {
    let p = project("group", true);
    p.mark_frontend("page");
    let output = p.run(&["--group", "std,page", "--check"]);
    assert_refused(&p, &output, "page", "set a design reviewer in Settings");
}

#[test]
fn a_frontend_run_of_a_project_without_a_design_table_is_refused_with_the_design_settings_message() {
    let p = project("no-table", false);
    p.fake_pi();
    p.design_reviewer("openai/gpt-6.1-sol");
    p.mark_frontend("page");
    let output = p.run(&["--feature", "page", "--check"]);
    assert_refused(&p, &output, "page", "this frontend feature's project has no design settings: add a [design] table to .orq/run.toml");
}

#[test]
fn a_design_reviewer_on_a_pi_model_without_images_is_refused_with_that_reason() {
    let p = project("no-images", true);
    p.fake_pi();
    p.design_reviewer("openai/text-only");
    p.mark_frontend("page");
    let output = p.run(&["--feature", "page", "--check"]);
    assert_refused(&p, &output, "page", "openai/text-only doesn't accept images, which the Design");
}

#[test]
fn a_standard_only_run_is_not_checked() {
    // No [design] table and no design agents: nothing frontend, so it starts as before.
    let p = project("standard", false);
    p.mark_frontend("page");
    let output = p.run(&["--group", "std", "--check"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let output = p.run(&["--feature", "std", "--check"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
}

#[test]
fn a_frontend_run_with_a_design_table_and_a_design_reviewer_passes_the_check() {
    let p = project("ready", true);
    p.fake_pi();
    p.design_reviewer("openai/gpt-6.1-sol");
    p.mark_frontend("page");
    let output = p.run(&["--feature", "page", "--check"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let output = p.run(&["--group", "std,page", "--check"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
}

#[test]
fn a_pi_design_reviewer_whose_models_cannot_be_listed_or_are_not_listed_is_refused() {
    let p = project("pi-listing", true);
    p.design_reviewer("openai/gpt-6.1-sol");
    p.mark_frontend("page");
    for (script, reason) in [
        // The listing fails, or says nothing that is a model list.
        ("#!/bin/sh\necho broken >&2\nexit 1\n", "pi's models could not be listed"),
        ("#!/bin/sh\nprintf 'no models here\\n'\n", "pi doesn't list openai/gpt-6.1-sol"),
        // The model the design reviewer names is not among them.
        ("#!/bin/sh\nprintf 'provider  model  context  max-out  thinking  images\\nopenai  other  400K  128K  yes  yes\\n'\n", "pi doesn't list openai/gpt-6.1-sol"),
    ] {
        p.pi_script(script);
        let output = p.run(&["--feature", "page", "--check"]);
        assert_refused(&p, &output, "page", reason);
    }
}

#[test]
fn a_pi_listing_without_an_images_column_is_refused() {
    let p = project("pi-no-images-column", true);
    p.design_reviewer("openai/gpt-6.1-sol");
    p.mark_frontend("page");
    // The model is listed, but whether it accepts images is unknown.
    p.pi_script("#!/bin/sh\nprintf 'provider  model  context  max-out  thinking\\nopenai  gpt-6.1-sol  400K  128K  yes\\n'\n");
    let output = p.run(&["--feature", "page", "--check"]);
    assert_refused(&p, &output, "page", "pi doesn't list openai/gpt-6.1-sol");
}

#[test]
fn a_pi_design_reviewer_without_pi_installed_is_refused() {
    let p = project("pi-missing", true);
    p.design_reviewer("openai/gpt-6.1-sol");
    p.mark_frontend("page");
    // The run's PATH has git and nothing else, so no pi installed on this machine is found.
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap()).map(|dir| dir.join("git")).find(|git| git.is_file()).unwrap();
    let only_git = p.home.join("only-git");
    std::fs::create_dir_all(&only_git).unwrap();
    std::os::unix::fs::symlink(git, only_git.join("git")).unwrap();
    let output = p
        .command()
        .args(["run", "--repo"])
        .arg(p.repo.root())
        .args(["--remote", "origin", "--target", "main", "--feature", "page", "--check"])
        .env("PATH", &only_git)
        .output()
        .unwrap();
    assert_refused(&p, &output, "page", "pi's models could not be listed");
}

#[test]
fn a_pi_listing_that_never_ends_is_bounded_and_refused() {
    let p = project("pi-hangs", true);
    p.design_reviewer("openai/gpt-6.1-sol");
    p.mark_frontend("page");
    p.pi_script("#!/bin/sh\nexec sleep 60\n");
    let started = std::time::Instant::now();
    let output = p.run(&["--feature", "page", "--check"]);
    assert!(started.elapsed() < std::time::Duration::from_secs(30), "took {:?}", started.elapsed());
    assert_refused(&p, &output, "page", "pi's models could not be listed");
}
