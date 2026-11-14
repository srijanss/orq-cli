//! A discard, and the preview before it, read the worktree's status once: a worktree with installed
//! packages lists thousands of files, so a second full scan doubles the wait. Its own test binary: it
//! puts a git that counts `status` calls on `PATH`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use orq_orchestrator::{discard_preview, discard_run, Event, RunRecord, RunStore};
use orq_test_support::GitFixture;

/// Puts `PATH` back as it was when dropped, also when the test panics.
struct RestorePath(OsString);

impl Drop for RestorePath {
    fn drop(&mut self) {
        std::env::set_var("PATH", &self.0);
    }
}

/// From now until the guard drops, every `git status` adds a line to `log`; git itself runs as usual.
fn git_that_counts_status(bin: &Path, log: &Path) -> RestorePath {
    let path = std::env::var_os("PATH").unwrap();
    let real = std::env::split_paths(&path).map(|dir| dir.join("git")).find(|git| git.is_file()).expect("a git on PATH");
    fs::create_dir_all(bin).unwrap();
    let wrapper = bin.join("git");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in status) echo status >> '{}';; esac; done\nexec '{}' \"$@\"\n",
            log.display(),
            real.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin.to_path_buf()];
    dirs.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(dirs).unwrap());
    RestorePath(path)
}

fn event(kind: &str) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("e-{kind}"),
        run_id: "run-1".into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        kind: kind.into(),
        stage: None,
        attempt: 1,
        occurred_at: "2026-11-02T10:00:00Z".into(),
        payload: serde_json::json!({}),
    }
}

fn status_calls(log: &Path) -> usize {
    fs::read_to_string(log).map(|text| text.lines().count()).unwrap_or(0)
}

#[test]
fn a_discard_and_its_preview_each_read_the_worktree_status_once() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("orq-run-1");
    let root: PathBuf = orq_test_support::process_temp_dir().join(format!("orq-discard-scans-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let store = RunStore::new(&root.join("state"));
    store
        .start_run(&RunRecord {
            delivery: Default::default(),
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "orq-run-1".into(),
            worktree: worktree.clone(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    store.append_event(&event("run.failed")).unwrap();
    let log = root.join("status.log");
    let _git = git_that_counts_status(&root.join("bin"), &log);

    discard_preview(&store, "run-1", repo.root()).unwrap();
    assert_eq!(status_calls(&log), 1, "the preview");

    let outcome = discard_run(&store, "run-1", repo.root()).unwrap();
    assert_eq!(outcome.kept_worktree, None);
    assert_eq!(status_calls(&log), 2, "the discard");
}
