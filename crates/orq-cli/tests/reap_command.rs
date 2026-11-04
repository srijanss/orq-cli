use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo(tag: &str) -> (PathBuf, PathBuf) {
    let root = orq_test_support::process_temp_dir().join(format!("orq-reap-cli-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["remote", "add", "origin", "https://github.example/o/r.git"][..]] {
        assert!(Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap().status.success());
    }
    (root, repo)
}

fn reap(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    command.args(["runs", "reap"]).args(args);
    for (name, dir) in [("XDG_STATE_HOME", "state"), ("XDG_DATA_HOME", "data"), ("XDG_CONFIG_HOME", "config"), ("HOME", "home")] {
        command.env(name, root.join(dir));
    }
    command.output().unwrap()
}

#[test]
fn reaping_with_nothing_to_reap_succeeds_quietly() {
    let (root, repo) = repo("empty");
    for extra in [&[][..], &["--dry-run"][..], &["--yes"][..]] {
        let mut args = vec!["--repo", repo.to_str().unwrap()];
        args.extend_from_slice(extra);
        let out = reap(&root, &args);
        assert!(out.status.success(), "{extra:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout.is_empty(), "{extra:?}");
    }
}

#[test]
fn reap_refuses_missing_repo_unknown_flags_and_contradictory_modes() {
    let (root, repo) = repo("usage");
    let repo = repo.to_str().unwrap();
    for args in [&[][..], &["--yes"][..], &["--repo", repo, "--force"][..], &["--repo", repo, "--yes", "--dry-run"][..], &["--repo"][..]] {
        let out = reap(&root, args);
        assert!(!out.status.success(), "{args:?} must be refused");
    }
}

#[test]
fn reap_removes_an_orphaned_private_ref_only_with_yes() {
    let (root, repo) = repo("orphan-ref");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["commit", "-q", "--allow-empty", "-m", "base"]);
    git(&["update-ref", "refs/orq/rebase/aa/end", "HEAD"]);
    let repo_arg = repo.to_str().unwrap();

    let dry = reap(&root, &["--repo", repo_arg]);
    assert!(dry.status.success());
    assert_eq!(String::from_utf8_lossy(&dry.stdout), "would remove ref\trefs/orq/rebase/aa/end\n");
    assert!(!git(&["for-each-ref", "refs/orq"]).is_empty(), "nothing is removed without --yes");

    let done = reap(&root, &["--repo", repo_arg, "--yes"]);
    assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    assert_eq!(String::from_utf8_lossy(&done.stdout), "removed ref\trefs/orq/rebase/aa/end\n");
    assert!(git(&["for-each-ref", "refs/orq"]).is_empty());
}
