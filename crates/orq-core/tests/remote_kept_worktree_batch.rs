//! `POST /api/storage/worktrees/delete-merged` hands the provider every listed worktree in one call, so a
//! provider can look its projects and runs up once for the whole batch, and still answers and audits each
//! worktree on its own.

use std::sync::Mutex;

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with_body, ProjectsProvider, RemoteConfig, Request, WorktreeDelete};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

#[derive(Default)]
struct Fake {
    batches: Mutex<Vec<(Vec<(String, String, String)>, bool)>>,
    singles: Mutex<usize>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn delete_kept_worktree(&self, _project: &str, _branch: &str, _version: &str, _force: bool) -> Result<WorktreeDelete, String> {
        *self.singles.lock().unwrap() += 1;
        Ok(WorktreeDelete::Unknown)
    }

    fn delete_kept_worktrees(&self, items: &[(&str, &str, &str)], force: bool) -> Vec<Result<WorktreeDelete, String>> {
        self.batches.lock().unwrap().push((items.iter().map(|(p, b, v)| (p.to_string(), b.to_string(), v.to_string())).collect(), force));
        items
            .iter()
            .map(|(project, branch, _)| match *branch {
                "fix--A" => Ok(WorktreeDelete::Deleted(json!({ "project": project, "branch": branch }))),
                "fix--B" => Ok(WorktreeDelete::Refused("the worktree changed since you opened it; nothing was deleted".into())),
                "fix--C" => Err("could not read /home/someone/.claude/worktrees/fix--C".into()),
                _ => Ok(WorktreeDelete::Unknown),
            })
            .collect()
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-kept-batch-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    roots
}

#[test]
fn delete_merged_makes_one_batch_call_and_answers_and_audits_each_worktree() {
    let roots = roots("one-call");
    let fake = Fake::default();
    let body = json!({ "worktrees": [
        { "project": "alpha", "branch": "fix--A", "version": "v1" },
        { "project": "beta", "branch": "fix--B", "version": "v2" },
        { "project": "beta", "branch": "fix--C", "version": "v3" },
        { "project": "gamma", "branch": "fix--D", "version": "v4" },
    ] })
    .to_string();
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "POST", path: "/api/storage/worktrees/delete-merged", peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };

    let r = handle_with_body(&roots, &config, "p", "p", &request, body.as_bytes(), &fake);

    assert_eq!(r.status, 200, "{}", r.body);
    let batches = fake.batches.lock().unwrap();
    assert_eq!(batches.len(), 1, "one call for the whole batch");
    let (items, force) = &batches[0];
    assert!(!force, "delete-merged never forces");
    let branches: Vec<&str> = items.iter().map(|(_, branch, _)| branch.as_str()).collect();
    assert_eq!(branches, ["fix--A", "fix--B", "fix--C", "fix--D"]);
    assert_eq!(items[1], ("beta".to_string(), "fix--B".to_string(), "v2".to_string()));
    assert_eq!(*fake.singles.lock().unwrap(), 0, "no worktree is deleted one call at a time");

    let results = r.body["results"].as_array().unwrap();
    let deleted: Vec<bool> = results.iter().map(|result| result["deleted"].as_bool().unwrap()).collect();
    assert_eq!(deleted, [true, false, false, false]);
    assert_eq!(results[1]["error"], "the worktree changed since you opened it; nothing was deleted");
    assert_eq!(results[2]["error"], "the worktree could not be deleted", "a path on this machine is never shown");
    assert_eq!(results[3]["error"], "not found");

    let audit = remote_audit::read(&roots, 20).unwrap();
    let merged: Vec<&Value> = audit.iter().filter(|line| line.to_string().contains("storage.worktree.delete-merged")).collect();
    assert_eq!(merged.len(), 4, "{audit:?}");
}
