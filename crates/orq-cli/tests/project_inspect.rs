use std::path::Path;
use std::process::{Command, Output};

use orq_test_support::GitFixture;

fn orq(args: &[&str], cwd_arg: Option<&Path>, pane_dir: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    command.args(args).env_remove("HERDR_ACTIVE_PANE_CWD");
    if let Some(cwd) = cwd_arg {
        command.arg("--cwd").arg(cwd);
    }
    if let Some(pane) = pane_dir {
        command.env("HERDR_ACTIVE_PANE_CWD", pane);
    }
    command.output().unwrap()
}

fn inspect_json(output: &Output) -> serde_json::Value {
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).expect("stdout is JSON")
}

#[test]
fn inspect_reports_each_repository_independently_as_json() {
    let repo_a = GitFixture::new();
    let repo_b = GitFixture::new();

    let a = inspect_json(&orq(&["project", "inspect"], Some(repo_a.nested_dir()), None));
    let b = inspect_json(&orq(&["project", "inspect"], Some(repo_b.nested_dir()), None));

    let root_a = repo_a.root().canonicalize().unwrap();
    let root_b = repo_b.root().canonicalize().unwrap();
    assert_eq!(a["name"], "repo");
    assert_eq!(a["root"], root_a.to_str().unwrap());
    assert_eq!(a["git_common_dir"], root_a.join(".git").to_str().unwrap());
    assert_eq!(b["root"], root_b.to_str().unwrap());
    assert_eq!(b["git_common_dir"], root_b.join(".git").to_str().unwrap());
    assert_ne!(a["root"], b["root"]);
}

#[test]
fn without_cwd_the_herdr_active_pane_directory_is_used() {
    let repo = GitFixture::new();

    let json = inspect_json(&orq(&["project", "inspect"], None, Some(repo.nested_dir())));

    assert_eq!(json["root"], repo.root().canonicalize().unwrap().to_str().unwrap());
}
