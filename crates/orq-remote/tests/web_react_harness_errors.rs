//! The jsdom harness fails when the page raises an error, so a crashed page is never mistaken for an empty one.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Runs the harness on a page script, or None when node or the web-src dependencies are missing.
fn run_page(tag: &str, script: &str) -> Option<Output> {
    if !crate_dir().join("web-src/node_modules/jsdom").exists() {
        eprintln!("skipped: node or the web-src dependencies are not installed");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("orq-harness-errors-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let page = dir.join("page.js");
    std::fs::write(&page, script).unwrap();
    let scenario = r#"{"responses": [], "steps": []}"#;
    match Command::new("node").arg(crate_dir().join("tests/web_react_harness.mjs")).arg(&page).arg(scenario).output() {
        Ok(output) => Some(output),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipped: node is not installed");
            None
        }
        Err(error) => panic!("cannot run node: {error}"),
    }
}

#[test]
fn a_page_that_works_runs_to_the_end_with_a_result() {
    let Some(output) = run_page("fine", "document.getElementById('app').textContent = 'ready';") else { return };
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"frames\""));
}

#[test]
fn an_error_raised_later_by_the_page_fails_the_run_and_prints_no_result() {
    let Some(output) = run_page("late", "setTimeout(function () { throw new Error('late boom'); }, 1);") else { return };
    assert!(!output.status.success(), "a page error must fail the harness");
    assert!(String::from_utf8_lossy(&output.stderr).contains("late boom"), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.stdout.is_empty(), "no result is printed for a crashed page");
}

#[test]
fn an_error_raised_while_the_page_loads_fails_the_run_too() {
    let Some(output) = run_page("load", "throw new Error('load boom');") else { return };
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("load boom"), "{}", String::from_utf8_lossy(&output.stderr));
}
