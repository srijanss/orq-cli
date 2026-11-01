use std::path::Path;
use std::process::Command;

use orq_test_support::FakeProvider;

fn run(program: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(program).args(args).current_dir(cwd).output().unwrap()
}

#[test]
fn gh_and_glab_return_their_scripted_json_and_every_call_is_recorded_in_order() {
    let provider = FakeProvider::builder()
        .reply(&["pr", "create"], "{\"url\":\"https://github.example/o/r/pull/7\",\"number\":7}\n")
        .reply(&["mr", "list"], "[{\"iid\":3,\"web_url\":\"https://gitlab.example/o/r/-/merge_requests/3\"}]\n")
        .build();
    let cwd = std::env::temp_dir();

    let created = run(&provider.gh(), &cwd, &["pr", "create", "--title", "it's a title", "--body", ""]);
    let listed = run(&provider.glab(), &cwd, &["mr", "list", "--source-branch", "fix--a"]);

    assert_eq!(created.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&created.stdout),
        "{\"url\":\"https://github.example/o/r/pull/7\",\"number\":7}\n"
    );
    assert_eq!(listed.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&listed.stdout).contains("merge_requests/3"));

    let calls = provider.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].program, "gh");
    assert_eq!(calls[0].args, ["pr", "create", "--title", "it's a title", "--body", ""]);
    assert_eq!(calls[0].cwd.canonicalize().unwrap(), cwd.canonicalize().unwrap());
    assert_eq!(calls[1].program, "glab");
    assert_eq!(calls[1].args, ["mr", "list", "--source-branch", "fix--a"]);
}

#[test]
fn the_most_specific_scripted_prefix_wins_whatever_the_registration_order() {
    let provider = FakeProvider::builder()
        .reply(&["pr"], "generic\n")
        .reply(&["pr", "view"], "specific\n")
        .build();
    let cwd = std::env::temp_dir();

    assert_eq!(String::from_utf8_lossy(&run(&provider.gh(), &cwd, &["pr", "view", "7"]).stdout), "specific\n");
    assert_eq!(String::from_utf8_lossy(&run(&provider.gh(), &cwd, &["pr", "list"]).stdout), "generic\n");
}

#[test]
fn a_scripted_failure_exits_with_its_code_and_stderr() {
    let provider = FakeProvider::builder()
        .fail(&["pr", "create"], 4, "HTTP 401: bad credentials\n")
        .build();

    let output = run(&provider.gh(), &std::env::temp_dir(), &["pr", "create"]);

    assert_eq!(output.status.code(), Some(4));
    assert_eq!(String::from_utf8_lossy(&output.stderr), "HTTP 401: bad credentials\n");
    assert!(output.stdout.is_empty());
}

#[test]
fn an_unscripted_call_fails_loudly_and_is_still_recorded() {
    let provider = FakeProvider::builder().reply(&["pr", "create"], "{}\n").build();

    let output = run(&provider.glab(), &std::env::temp_dir(), &["mr", "merge", "3"]);

    assert_eq!(output.status.code(), Some(64));
    assert!(String::from_utf8_lossy(&output.stderr).contains("no scripted reply for: mr merge 3"));
    assert_eq!(provider.calls().len(), 1);
    assert_eq!(provider.calls()[0].args, ["mr", "merge", "3"]);
}

#[test]
fn a_provider_that_was_never_called_has_no_calls_and_the_bin_dir_holds_both_clis() {
    let provider = FakeProvider::builder().build();

    assert!(provider.calls().is_empty());
    assert_eq!(provider.gh(), provider.bin_dir().join("gh"));
    assert_eq!(provider.glab(), provider.bin_dir().join("glab"));
    assert!(provider.gh().is_file() && provider.glab().is_file());
}

#[test]
fn found_through_path_it_works_and_the_directory_is_removed_on_drop() {
    let provider = FakeProvider::builder().reply(&["auth", "status"], "logged in\n").build();
    let bin = provider.bin_dir();
    let mut dirs = vec![bin.clone()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    let output = Command::new("gh")
        .args(["auth", "status"])
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .output()
        .unwrap();

    assert_eq!(String::from_utf8_lossy(&output.stdout), "logged in\n");
    assert_eq!(provider.calls()[0].program, "gh");
    drop(provider);
    assert!(!bin.exists());
}
