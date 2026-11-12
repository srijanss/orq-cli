//! Shared by the browser-behaviour tests: whether node and the locked web dependencies are there.
//! Without node the tests skip. With node but without `npm ci` they fail, so a checkout that
//! forgot the install cannot pass with nothing checked; `ORQ_SKIP_WEB_TESTS=1` opts out on purpose.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Run,
    Skip,
    Fail(String),
}

pub fn decide(node: bool, dependencies: bool, opted_out: bool) -> Decision {
    if !node {
        return Decision::Skip;
    }
    if dependencies {
        return Decision::Run;
    }
    if opted_out {
        return Decision::Skip;
    }
    Decision::Fail("node is installed but the web dependencies are not: run `npm ci` in crates/orq-remote/web-src, or set ORQ_SKIP_WEB_TESTS=1 to skip the browser tests on purpose".to_string())
}

#[derive(Debug, PartialEq, Eq)]
pub enum NodeProbe {
    Present,
    Absent,
    Broken(String),
}

/// Only a node that is not installed counts as absent. A node that cannot be started for any
/// other reason, or that exits with an error for `--version`, is a broken test environment.
pub fn classify_node_probe(result: Result<bool, std::io::ErrorKind>) -> NodeProbe {
    match result {
        Ok(true) => NodeProbe::Present,
        Ok(false) => NodeProbe::Broken("node is installed but `node --version` failed".to_string()),
        Err(std::io::ErrorKind::NotFound) => NodeProbe::Absent,
        Err(kind) => NodeProbe::Broken(format!("node could not be started ({kind}); fix the environment or set ORQ_SKIP_WEB_TESTS=1")),
    }
}

/// True when the test should run. Prints why when it skips; panics when the policy says fail.
pub fn ready() -> bool {
    let probe = Command::new("node").arg("--version").output().map(|output| output.status.success()).map_err(|error| error.kind());
    let opted_out = std::env::var("ORQ_SKIP_WEB_TESTS").map(|value| value == "1").unwrap_or(false);
    let node = match classify_node_probe(probe) {
        NodeProbe::Present => true,
        NodeProbe::Absent => false,
        NodeProbe::Broken(message) if opted_out => {
            eprintln!("skipped: {message}");
            return false;
        }
        NodeProbe::Broken(message) => panic!("{message}"),
    };
    let dependencies = Path::new(env!("CARGO_MANIFEST_DIR")).join("web-src/node_modules/jsdom").exists();
    match decide(node, dependencies, opted_out) {
        Decision::Run => true,
        Decision::Skip => {
            eprintln!("skipped: node or the web-src dependencies are not available");
            false
        }
        Decision::Fail(message) => panic!("{message}"),
    }
}

/// Makes one new directory under `base`, owner-only. It fails when the name already exists,
/// as a directory, a file or a link someone planted, so a guessed path is never reused or followed.
pub fn create_private_dir(base: &Path, name: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let path = base.join(name);
    std::fs::DirBuilder::new().mode(0o700).create(&path)?;
    Ok(path)
}

/// A fresh private directory in the system temp dir with an unpredictable name.
pub fn private_temp_dir(tag: &str) -> PathBuf {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    for _ in 0..16 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u32(std::process::id());
        let name = format!("orq-web-{tag}-{:016x}", hasher.finish());
        match create_private_dir(&orq_test_support::process_temp_dir(), &name) {
            Ok(path) => return path,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("cannot create a private temp dir: {error}"),
        }
    }
    panic!("cannot find an unused private temp dir name");
}
