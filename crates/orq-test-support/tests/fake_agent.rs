use std::process::Command;

use orq_test_support::FakeAgent;

#[test]
fn records_argv_and_cwd_prints_output_and_exits_with_configured_code() {
    let agent = FakeAgent::builder().stdout("hello from fake\n").exit_code(3).build();
    let workdir = std::env::temp_dir();

    let output = Command::new(agent.path())
        .args(["--model", "x", "task one"])
        .current_dir(&workdir)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello from fake\n");
    let run = agent.last_run().expect("a recorded run");
    assert_eq!(run.args, vec!["--model", "x", "task one"]);
    assert_eq!(
        run.cwd.canonicalize().unwrap(),
        workdir.canonicalize().unwrap()
    );
}

#[test]
fn a_hanging_agent_keeps_running_until_it_is_killed() {
    let agent = FakeAgent::builder().hang().build();

    let mut child = Command::new(agent.path()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(child.try_wait().unwrap().is_none(), "agent exited on its own");

    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert!(!status.success());
    assert!(agent.last_run().is_some(), "argv/cwd recorded before hanging");
}
