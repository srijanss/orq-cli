use std::path::PathBuf;
use std::process::{Command, Output};

struct Sandbox {
    home: PathBuf,
    repo: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let home = orq_test_support::process_temp_dir().join(format!("orq-features-archive-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let repo = home.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(Command::new("git").args(["init", "-q"]).arg(&repo).status().unwrap().success());
        Sandbox { home, repo }
    }

    fn orq(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
        command.args(args);
        command.env("HOME", &self.home);
        command.env("XDG_DATA_HOME", self.home.join("data"));
        command.env("XDG_STATE_HOME", self.home.join("state"));
        command.env("XDG_CONFIG_HOME", self.home.join("config"));
        command.env("XDG_CACHE_HOME", self.home.join("cache"));
        command.env_remove("HERDR_ACTIVE_PANE_CWD");
        command.output().unwrap()
    }

    fn ledger(&self) -> PathBuf {
        self.repo.join(".tdd-features.json")
    }

    fn write_ledger(&self, names: &[(&str, &str, &[&str])]) {
        let entries: Vec<serde_json::Value> = names
            .iter()
            .map(|(name, status, deps)| serde_json::json!({"featureName": name, "status": status, "dependsOn": deps}))
            .collect();
        std::fs::write(self.ledger(), serde_json::to_string_pretty(&entries).unwrap()).unwrap();
    }

    fn ledger_names(&self) -> Vec<String> {
        let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(self.ledger()).unwrap()).unwrap();
        value.as_array().unwrap().iter().map(|e| e["featureName"].as_str().unwrap().to_string()).collect()
    }
}

fn text(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

const LEDGER: &[(&str, &str, &[&str])] =
    &[("c1", "completed", &[]), ("c2", "completed", &["c1"]), ("p1", "pending", &["c1"]), ("c3", "completed", &[])];

#[test]
fn features_archive_moves_old_completed_features_and_reports_counts() {
    let sandbox = Sandbox::new("moves");
    sandbox.write_ledger(LEDGER);
    let repo = sandbox.repo.to_str().unwrap();

    let out = sandbox.orq(&["features", "archive", "--repo", repo, "--keep", "1"]);

    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("archived 2"), "{}", text(&out));
    assert_eq!(sandbox.ledger_names(), vec!["p1", "c3"]);
    let archives: Vec<_> = glob_archive(&sandbox);
    assert_eq!(archives.len(), 1, "{archives:?}");
}

#[test]
fn feature_edit_finds_an_archived_feature_read_only_and_add_drops_an_archived_dependency_with_a_note() {
    let sandbox = Sandbox::new("edit-archived");
    sandbox.write_ledger(LEDGER);
    let repo = sandbox.repo.to_str().unwrap();
    assert!(sandbox.orq(&["features", "archive", "--repo", repo, "--keep", "1"]).status.success());

    let out = sandbox.orq(&["feature", "edit", "--repo", repo, "--name", "c1", "--description", "x"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("c1 is archived and completed: it is read-only"), "{}", text(&out));

    let out = sandbox.orq(&["feature", "add", "--repo", repo, "--name", "fresh", "--depends-on", "c1"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("c1 is archived and completed: not needed"), "{}", text(&out));
    let ledger = std::fs::read_to_string(sandbox.ledger()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&ledger).unwrap();
    let fresh = value.as_array().unwrap().iter().find(|e| e["featureName"] == "fresh").unwrap();
    assert!(fresh.get("dependsOn").map_or(true, |d| d.as_array().unwrap().is_empty()), "{fresh}");
}

/// A name from the ledger that carries a terminal escape never reaches the terminal raw in a note or a count line.
#[test]
fn archived_names_with_control_characters_are_shown_escaped() {
    let sandbox = Sandbox::new("escapes");
    let weird = "c\u{1b}[31mred";
    sandbox.write_ledger(&[(weird, "completed", &[]), ("p1", "pending", &[weird]), ("c2", "completed", &[])]);
    let repo = sandbox.repo.to_str().unwrap();

    let dry = sandbox.orq(&["features", "archive", "--repo", repo, "--keep", "1", "--dry-run"]);
    assert!(dry.status.success(), "{}", text(&dry));
    assert!(!text(&dry).contains('\u{1b}'), "{:?}", text(&dry));
    assert!(sandbox.orq(&["features", "archive", "--repo", repo, "--keep", "1"]).status.success());

    let out = sandbox.orq(&["feature", "add", "--repo", repo, "--name", "fresh", "--depends-on", weird]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!text(&out).contains('\u{1b}'), "{:?}", text(&out));
    assert!(text(&out).contains("is archived and completed: not needed"), "{}", text(&out));
}

fn glob_archive(sandbox: &Sandbox) -> Vec<PathBuf> {
    let root = sandbox.home.join("data/orq/archive");
    let Ok(dir) = std::fs::read_dir(&root) else { return Vec::new() };
    dir.flatten().map(|e| e.path().join("features.json")).filter(|p| p.exists()).collect()
}

#[test]
fn features_archive_dry_run_prints_counts_and_stripped_names_and_writes_nothing() {
    let sandbox = Sandbox::new("dry");
    sandbox.write_ledger(LEDGER);
    let before = std::fs::read_to_string(sandbox.ledger()).unwrap();

    let out = sandbox.orq(&["features", "archive", "--repo", sandbox.repo.to_str().unwrap(), "--keep", "1", "--dry-run"]);

    assert!(out.status.success(), "{}", text(&out));
    let shown = text(&out);
    assert!(shown.contains("would archive 2"), "{shown}");
    assert!(shown.contains("p1") && shown.contains("c1"), "{shown}");
    assert_eq!(std::fs::read_to_string(sandbox.ledger()).unwrap(), before);
    assert!(glob_archive(&sandbox).is_empty());
}
