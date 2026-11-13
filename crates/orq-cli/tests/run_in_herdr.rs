//! `orq run --detach --in-herdr`, how the web starts a run: when Herdr is running, the run goes in
//! the foreground of a new Herdr pane, so its implementer's Claude session is there to watch and
//! answer, instead of running unseen in the background.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

fn scratch(tag: &str) -> PathBuf {
    let root = orq_test_support::process_temp_dir().join(format!("orq-run-in-herdr-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config/orq")).unwrap();
    std::fs::create_dir_all(root.join("state")).unwrap();
    root
}

/// A stand-in for `herdr`: logs each call, names a new tab's pane, and keeps the command a pane is
/// asked to run without running it.
fn fake_herdr(dir: &Path) -> PathBuf {
    let path = dir.join("herdr");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {calls}\ncase \"$1 $2\" in\n\"tab create\") echo '{{\"result\":{{\"root_pane\":{{\"workspace_id\":\"w1\",\"tab_id\":\"t1\",\"pane_id\":\"p1\"}}}}}}';;\n\"pane run\") printf '%s' \"$4\" > {command};;\nesac\n",
        calls = dir.join("calls.log").display(),
        command = dir.join("command.txt").display(),
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_run_started_for_the_web_while_herdr_is_running_runs_in_the_foreground_of_a_herdr_pane() {
    let repo = GitFixture::new();
    assert!(Command::new("git").arg("-C").arg(repo.root()).args(["remote", "add", "origin", "git@github.com:o/r.git"]).status().unwrap().success());
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let dir = scratch("pane");
    let herdr = fake_herdr(&dir);

    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--detach", "--in-herdr"])
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("HERDR_BIN_PATH", &herdr)
        .env("HERDR_SOCKET_PATH", dir.join("herdr.sock"))
        .env("HERDR_WORKSPACE_ID", "w1")
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    let run_id = stdout.lines().find_map(|line| line.strip_prefix("run: ")).unwrap_or_else(|| panic!("no run id in {stdout}")).to_string();
    assert!(stdout.contains("state: launched"), "{stdout}");

    let calls = std::fs::read_to_string(dir.join("calls.log")).unwrap();
    let create = calls.lines().next().unwrap();
    assert!(create.starts_with("tab create") && create.contains("--workspace w1") && create.contains(&repo.root().display().to_string()), "{calls}");
    let command = std::fs::read_to_string(dir.join("command.txt")).unwrap();
    // The pane runs `exec sh -c <script>`; the script, unquoted, is what runs the run.
    let quoted = command.strip_prefix("exec sh -c ").unwrap_or_else(|| panic!("{command}"));
    let command = text(&Command::new("sh").args(["-c", &format!("printf %s {quoted}")]).output().unwrap().stdout);
    for wanted in ["'run'", "'--feature' 'COM-01'", &format!("'--run-id' '{run_id}'"), "XDG_STATE_HOME="] {
        assert!(command.contains(wanted), "{wanted} in {command}");
    }
    assert!(!command.contains("--detach") && !command.contains("--in-herdr"), "the pane runs it in the foreground: {command}");
    assert!(!dir.join("state/orq/logs").join(&run_id).join("runner.log").exists() && !dir.join("state/logs").join(&run_id).join("runner.log").exists(), "nothing runs detached");
}
