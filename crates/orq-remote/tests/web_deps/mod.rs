//! Shared by the browser-behaviour tests: whether node and the locked web dependencies are there.
//! Without node the tests skip. With node but without `npm ci` they fail, so a checkout that
//! forgot the install cannot pass with nothing checked; `ORQ_SKIP_WEB_TESTS=1` opts out on purpose.
#![allow(dead_code)]

use std::path::Path;
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

/// True when the test should run. Prints why when it skips; panics when the policy says fail.
pub fn ready() -> bool {
    let node = Command::new("node").arg("--version").output().is_ok();
    let dependencies = Path::new(env!("CARGO_MANIFEST_DIR")).join("web-src/node_modules/jsdom").exists();
    let opted_out = std::env::var("ORQ_SKIP_WEB_TESTS").map(|value| value == "1").unwrap_or(false);
    match decide(node, dependencies, opted_out) {
        Decision::Run => true,
        Decision::Skip => {
            eprintln!("skipped: node or the web-src dependencies are not available");
            false
        }
        Decision::Fail(message) => panic!("{message}"),
    }
}
