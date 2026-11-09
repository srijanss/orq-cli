//! Whatever the settings, the web can only read, except for the three listed actions when they are
//! switched on: starting a run, pausing it and resuming it. Nothing else answers to a write.

use orq_core::remote_inbox::{handle_with, ProjectsProvider, RemoteConfig, Request, RunControl, RunControlOutcome, RunStart};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Counting {
    changes: AtomicUsize,
}

impl ProjectsProvider for Counting {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn start_run(&self, _id: &str, _feature: &str) -> Result<RunStart, String> {
        self.changes.fetch_add(1, Ordering::SeqCst);
        Ok(RunStart::Unknown)
    }

    fn control_run(&self, _id: &str, _run: &str, _control: RunControl) -> Result<RunControlOutcome, String> {
        self.changes.fetch_add(1, Ordering::SeqCst);
        Ok(RunControlOutcome::Unknown)
    }
}

const READ_PATHS: &[&str] = &[
    "/api/projects",
    "/api/projects/p",
    "/api/projects/p/features",
    "/api/projects/p/runs",
    "/api/projects/p/reviews",
    "/api/reviews",
    "/api/reviews/r-1",
    "/api/settings",
    "/api/audit",
    "/api/projects/p/runs/r-1",
    "/api/projects/p/runs/r-1/merge",
    "/api/projects/p/runs/r-1/approve",
    "/api/projects/p/runs/r-1/restart",
    "/api/projects/p/runs/r-1/stop",
    "/api/projects/p/features/f/approve",
    "/api/projects/p/features/f/edit",
    "/api/projects/p/features/f",
    "/api/projects/p/reviews/r-1/merge",
    "/api/reviews/r-1/approve",
    "/api/settings/remote_actions",
    "/api/audit/clear",
];

const ACTION_PATHS: &[&str] = &["/api/projects/p/features/f/runs", "/api/projects/p/runs/r-1/pause", "/api/projects/p/runs/r-1/resume"];

fn roots(tag: &str, actions: bool) -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-no-mutation-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    if actions {
        std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    }
    roots
}

fn status(roots: &StorageRoots, provider: &Counting, method: &str, path: &str) -> u16 {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    handle_with(roots, &config, "p", "p", &request, provider).status
}

#[test]
fn with_remote_actions_off_nothing_but_a_read_is_accepted_and_nothing_changes() {
    let roots = roots("off", false);
    let provider = Counting::default();
    for path in READ_PATHS.iter().chain(ACTION_PATHS) {
        for method in ["POST", "PUT", "DELETE", "PATCH"] {
            let found = status(&roots, &provider, method, path);
            assert!(found == 404 || found == 405, "{method} {path} answered {found}");
        }
    }
    assert_eq!(provider.changes.load(Ordering::SeqCst), 0);
}

#[test]
fn with_remote_actions_on_only_the_three_actions_accept_a_post_and_no_other_method_changes_anything() {
    let roots = roots("on", true);
    let provider = Counting::default();
    for path in READ_PATHS {
        for method in ["POST", "PUT", "DELETE", "PATCH"] {
            let found = status(&roots, &provider, method, path);
            assert!(found == 404 || found == 405, "{method} {path} answered {found}");
        }
    }
    for path in ACTION_PATHS {
        for method in ["PUT", "DELETE", "PATCH"] {
            assert_eq!(status(&roots, &provider, method, path), 405, "{method} {path}");
        }
    }
    assert_eq!(provider.changes.load(Ordering::SeqCst), 0, "nothing but a POST to an action route reaches the provider");
    for path in ACTION_PATHS {
        status(&roots, &provider, "POST", path);
    }
    assert_eq!(provider.changes.load(Ordering::SeqCst), ACTION_PATHS.len());
}
