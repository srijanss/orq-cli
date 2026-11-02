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

#[test]
fn concurrent_calls_with_large_arguments_are_each_recorded_intact() {
    use std::process::Stdio;

    const COUNT: usize = 48;
    const BASE: usize = 100_000;
    let provider = FakeProvider::builder().reply(&["pr", "create"], "{}\n").build();
    let cwd = std::env::temp_dir();
    let programs = [provider.gh(), provider.glab()];

    let children: Vec<_> = (0..COUNT)
        .map(|i| {
            let body = format!("{i:02}{}", "x".repeat(BASE + i));
            Command::new(&programs[i % 2])
                .args(["pr", "create", "--body", &body])
                .current_dir(&cwd)
                .stdout(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }

    let calls = provider.calls();
    assert_eq!(calls.len(), COUNT);
    let mut seen: Vec<usize> = calls
        .iter()
        .map(|call| {
            assert_eq!(call.args.len(), 4, "{:?}", call.program);
            assert_eq!(&call.args[..3], ["pr", "create", "--body"]);
            let body = &call.args[3];
            let i: usize = body[..2].parse().unwrap();
            assert_eq!(body.len(), 2 + BASE + i, "argument {i} was cut or mixed");
            assert!(body[2..].bytes().all(|b| b == b'x'), "argument {i} was mixed with another call's");
            assert_eq!(call.program, ["gh", "glab"][i % 2]);
            i
        })
        .collect();
    seen.sort_unstable();
    assert_eq!(seen, (0..COUNT).collect::<Vec<_>>());
}

fn calls_dir(provider: &FakeProvider) -> std::path::PathBuf {
    provider.bin_dir().parent().unwrap().join("data").join("calls")
}

#[test]
fn a_call_still_being_recorded_is_not_reported_but_complete_calls_around_it_are() {
    let provider = FakeProvider::builder().reply(&["pr", "create"], "{}\n").build();
    let cwd = std::env::temp_dir();
    run(&provider.gh(), &cwd, &["pr", "create", "--title", "first"]);

    // What a call leaves behind while (or after being killed while) it is
    // still writing: a claimed slot whose record stops partway.
    let dir = calls_dir(&provider);
    std::fs::write(dir.join("1"), b"3\0gh\0/tmp\0pr\0create\0--ti").unwrap();
    std::fs::write(dir.join("2"), b"2\0gh\0/tmp\0pr\0cre").unwrap();
    std::fs::write(dir.join("3"), b"2\0gh\0/tmp\0pr\0create\0").unwrap();
    std::fs::write(dir.join("4"), b"2\0gh\0/tmp\0pr\0create\0END").unwrap();
    run(&provider.glab(), &cwd, &["mr", "list"]);

    let calls = provider.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0].args, ["pr", "create", "--title", "first"]);
    assert_eq!(calls[1].program, "glab");
    assert_eq!(calls[1].args, ["mr", "list"]);
}

#[test]
fn a_call_that_cannot_record_itself_fails_with_a_diagnostic_instead_of_hanging() {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let provider = FakeProvider::builder().reply(&["pr", "create"], "{}\n").build();
    // As if the provider were dropped while a child was still starting.
    std::fs::remove_dir(calls_dir(&provider)).unwrap();

    let mut child = Command::new(provider.gh())
        .args(["pr", "create"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("the fake provider hung instead of failing");
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    assert_eq!(status.code(), Some(70));
    let mut stderr = String::new();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    assert!(stderr.contains("cannot record call"), "{stderr}");
}

#[test]
fn one_shot_replies_are_served_in_order_and_then_the_standing_reply_takes_over() {
    let provider = FakeProvider::builder()
        .reply_once(&["api"], "first\n")
        .fail_once(&["api"], 3, "boom\n")
        .reply(&["api"], "standing\n")
        .build();
    let cwd = std::env::temp_dir();

    let first = run(&provider.gh(), &cwd, &["api", "x"]);
    let second = run(&provider.gh(), &cwd, &["api", "x"]);
    let third = run(&provider.gh(), &cwd, &["api", "x"]);
    let fourth = run(&provider.gh(), &cwd, &["api", "x"]);

    assert_eq!(String::from_utf8_lossy(&first.stdout), "first\n");
    assert_eq!(second.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&second.stderr), "boom\n");
    assert_eq!(String::from_utf8_lossy(&third.stdout), "standing\n");
    assert_eq!(String::from_utf8_lossy(&fourth.stdout), "standing\n");
    assert_eq!(provider.calls().len(), 4);
}

#[test]
fn a_one_shot_reply_is_served_first_even_when_the_standing_reply_was_registered_before_it() {
    let provider = FakeProvider::builder().reply(&["api"], "standing\n").reply_once(&["api"], "once\n").build();
    let cwd = std::env::temp_dir();

    let first = run(&provider.gh(), &cwd, &["api", "x"]);
    let second = run(&provider.gh(), &cwd, &["api", "x"]);

    assert_eq!(String::from_utf8_lossy(&first.stdout), "once\n");
    assert_eq!(String::from_utf8_lossy(&second.stdout), "standing\n");
}

#[test]
fn a_slow_reply_takes_at_least_its_delay() {
    let provider = FakeProvider::builder().reply_slowly(&["api"], "late\n", 300).build();
    let started = std::time::Instant::now();

    let out = run(&provider.gh(), &std::env::temp_dir(), &["api"]);

    assert_eq!(String::from_utf8_lossy(&out.stdout), "late\n");
    assert!(started.elapsed() >= std::time::Duration::from_millis(280), "{:?}", started.elapsed());
}
