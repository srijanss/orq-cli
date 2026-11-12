//! Tests make their scratch directories inside `process_temp_dir()`, which is removed when the
//! test process exits, and never straight under the shared temp dir, where a killed or failed
//! test leaves them behind.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A directory made straight under the shared temp dir. `process_temp_dir().join(` does not match.
const SHARED: &str = "std::env::temp_dir().join(";

/// Files that may still name a path straight under the shared temp dir, and how many times.
const ALLOWED: [(&str, usize, &str); 4] = [
    ("crates/orq-test-support/src/lib.rs", 2, "it makes the process temp dir itself"),
    ("crates/orq-orchestrator/src/push_stage.rs", 1, "production scratch space, not a test"),
    ("crates/orq-cli/src/pipeline.rs", 1, "production scratch space, not a test; its tests use the process temp dir"),
    ("crates/orq-remote/tests/web_deps_policy.rs", 1, "only compared against, never made"),
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// How many times each file of every crate's `src` and `tests` names a path under the shared temp dir.
fn shared_temp_dir_uses(workspace: &Path) -> BTreeMap<String, usize> {
    let mut files = Vec::new();
    for krate in std::fs::read_dir(workspace.join("crates")).unwrap().flatten() {
        rust_files(&krate.path().join("src"), &mut files);
        rust_files(&krate.path().join("tests"), &mut files);
    }
    assert!(files.len() > 100, "the scan found only {} source files, so it is not looking where the code is", files.len());
    files
        .into_iter()
        .filter(|path| !path.ends_with("no_shared_temp_dirs.rs")) // the guard has to name what it forbids
        .filter_map(|path| {
            let uses = std::fs::read_to_string(&path).unwrap().matches(SHARED).count();
            let relative = path.strip_prefix(workspace).unwrap().to_string_lossy().into_owned();
            (uses > 0).then_some((relative, uses))
        })
        .collect()
}

#[test]
fn tests_make_their_directories_in_the_process_temp_dir_never_straight_under_the_shared_one() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let allowed: BTreeMap<String, usize> = ALLOWED.iter().map(|(path, uses, _)| (path.to_string(), *uses)).collect();
    assert_eq!(
        shared_temp_dir_uses(&workspace),
        allowed,
        "a file makes a directory straight under the shared temp dir: use orq_test_support::process_temp_dir().join(..) instead \
         (or, when an allowed use is gone, drop it from ALLOWED)"
    );
}
