//! The workspace builds its dev and test binaries with line tables only (and no debug information for
//! dependencies), so each run worktree's `target/` stays small, while failures still name files and lines.

use std::path::Path;

use toml_edit::DocumentMut;

fn workspace_manifest() -> DocumentMut {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    std::fs::read_to_string(&path)
        .unwrap()
        .parse::<DocumentMut>()
        .unwrap()
}

#[test]
fn the_dev_profile_keeps_line_tables_only_and_drops_dependency_debug_info() {
    let manifest = workspace_manifest();
    let profile = manifest.get("profile").and_then(|p| p.as_table_like());
    let dev = profile.and_then(|p| p.get("dev")).and_then(|d| d.as_table_like());

    assert_eq!(
        dev.and_then(|d| d.get("debug")).and_then(|v| v.as_str()),
        Some("line-tables-only"),
        "profile.dev.debug"
    );
    let all_packages = dev
        .and_then(|d| d.get("package"))
        .and_then(|p| p.as_table_like())
        .and_then(|p| p.get("*"))
        .and_then(|p| p.as_table_like());
    assert_eq!(
        all_packages.and_then(|p| p.get("debug")).and_then(|v| v.as_bool()),
        Some(false),
        "profile.dev.package.\"*\".debug"
    );
    let release_debug = profile
        .and_then(|p| p.get("release"))
        .and_then(|r| r.as_table_like())
        .and_then(|r| r.get("debug"));
    assert!(release_debug.is_none(), "profile.release.debug must not be set");
}

#[test]
fn a_backtrace_names_this_test_file_with_a_line_number() {
    let backtrace = std::backtrace::Backtrace::force_capture().to_string();

    let names_this_file_with_a_line = backtrace.lines().any(|line| {
        line.split("workspace_build_profile.rs:")
            .nth(1)
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    });
    assert!(
        names_this_file_with_a_line,
        "the backtrace has no workspace_build_profile.rs:<line> frame:\n{backtrace}"
    );
}

#[test]
fn a_caught_panic_reports_this_file_and_a_line() {
    use std::sync::{Arc, Mutex};

    let location = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&location);
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        *seen.lock().unwrap() = info
            .location()
            .map(|at| (at.file().to_owned(), at.line()));
    }));
    let outcome = std::panic::catch_unwind(|| panic!("a test panic"));
    std::panic::set_hook(previous_hook);

    assert!(outcome.is_err());
    let (file, line) = location.lock().unwrap().clone().expect("the hook saw no location");
    assert!(file.ends_with("workspace_build_profile.rs"), "panic file: {file}");
    assert!(line > 0, "panic line: {line}");
}
