//! Tests start orq with every folder it writes inside a temporary home, and a leaking start fails
//! instead of writing into the person's real orq folders.

use orq_test_support::{orq_command, TempHome};
use std::path::{Path, PathBuf};

/// A stand-in for the person's real HOME: outside the temporary directory, so the sandbox guard
/// treats it as real. Removed when dropped.
struct FakeRealHome(PathBuf);

impl FakeRealHome {
    fn new(name: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("fake-real-home-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        FakeRealHome(dir)
    }

    fn entries(&self) -> Vec<PathBuf> {
        std::fs::read_dir(&self.0).unwrap().map(|entry| entry.unwrap().path()).collect()
    }
}

impl Drop for FakeRealHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Under nextest (the gate and sessions) every test process gets `ORQ_TEST_SANDBOX=1` from orq's
/// `.config/nextest.toml`, so even a test that starts orq without the helper cannot leak.
#[test]
fn nextest_gives_this_test_the_sandbox_flag() {
    if std::env::var_os("NEXTEST").is_none() {
        return;
    }
    assert_eq!(std::env::var("ORQ_TEST_SANDBOX").as_deref(), Ok("1"));
}

#[test]
fn orq_nextest_config_sets_the_sandbox_flag_for_its_tests() {
    if std::env::var_os("NEXTEST").is_some() {
        return; // nextest_gives_this_test_the_sandbox_flag checks it directly.
    }
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let probe = std::process::Command::new(&cargo)
        .args(["nextest", "run", "-p", "orq-test-support", "--test", "sandboxed_orq", "--no-tests=fail", "-E", "test(=nextest_gives_this_test_the_sandbox_flag)"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env_remove("ORQ_TEST_SANDBOX")
        .output()
        .unwrap();
    assert!(probe.status.success(), "the probe failed under nextest: {}", String::from_utf8_lossy(&probe.stderr));
}

#[test]
fn a_leaking_start_fails_naming_the_real_folder_and_writes_nothing_there() {
    // What the leaking tests did: only the state folder moved into a temp dir.
    let real = FakeRealHome::new("leak");
    let state = orq_test_support::TempDir::new("orq-leak-state");
    let mut command = std::process::Command::new(orq_test_support::orq_binary());
    command
        .env("ORQ_TEST_SANDBOX", "1")
        .env("HOME", &real.0)
        .env("XDG_STATE_HOME", state.path())
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_CACHE_HOME");

    let output = command.args(["review", "backfill"]).output().unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "a leaking start must fail");
    let data = real.0.join(".local").join("share").join("orq");
    assert!(stderr.contains(&data.display().to_string()), "the error must name {data:?}: {stderr}");
    assert_eq!(real.entries(), Vec::<PathBuf>::new(), "orq wrote into the fake real HOME");
}

#[test]
fn the_helper_turns_on_the_sandbox_guard_for_what_it_starts() {
    let home = TempHome::new();
    let mut command = std::process::Command::new("sh");
    command.args(["-c", "printf %s \"$ORQ_TEST_SANDBOX\""]).env_remove("ORQ_TEST_SANDBOX");
    home.apply(&mut command);

    let output = command.output().unwrap();

    assert_eq!(String::from_utf8_lossy(&output.stdout), "1");
}

#[test]
fn a_sandboxed_command_points_every_orq_folder_into_the_temp_dir_and_keeps_home() {
    let script = "printf '%s\\n' \"$ORQ_TEST_SANDBOX\" \"$XDG_DATA_HOME\" \"$XDG_STATE_HOME\" \"$XDG_CONFIG_HOME\" \"$XDG_CACHE_HOME\" \"$HOME\"";
    let mut command = orq_test_support::sandboxed_command("sh");
    command.args(["-c", script]).env("HOME", "/home/someone");

    let output = command.output().unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "1");
    for folder in &lines[1..5] {
        assert!(Path::new(folder).starts_with(std::env::temp_dir()), "{folder:?} is not under the temp dir");
    }
    assert_eq!(lines[1..5].iter().collect::<std::collections::HashSet<_>>().len(), 4, "the folders must differ: {lines:?}");
    // HOME stays the person's, so git still finds its identity.
    assert_eq!(lines[5], "/home/someone");
}

#[test]
fn the_helper_keeps_orq_data_inside_the_temp_home() {
    let real = FakeRealHome::new("helper");
    let home = TempHome::new();
    let mut command = orq_command(&home);
    // The person's environment: a real HOME with no XDG folders of its own.
    command.env("HOME", &real.0);
    home.apply(&mut command);

    let output = command.args(["review", "backfill"]).output().unwrap();

    assert!(output.status.success(), "orq review backfill failed: {}", String::from_utf8_lossy(&output.stderr));
    assert!(home.data_home().join("orq").join("orq.db").is_file(), "no database under {:?}", home.data_home());
    assert!(home.path().starts_with(std::env::temp_dir()), "temp home {:?} is not under the temp dir", home.path());
    assert_eq!(real.entries(), Vec::<PathBuf>::new(), "orq wrote into the fake real HOME");
}
