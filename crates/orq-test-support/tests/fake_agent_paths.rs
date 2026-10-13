use std::process::Command;

use orq_test_support::FakeAgent;

#[test]
fn works_when_the_temp_dir_path_contains_an_apostrophe() {
    let tmp = std::env::temp_dir().join(format!("o'brien-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    std::env::set_var("TMPDIR", &tmp);

    let agent = FakeAgent::builder().stdout("ok\n").build();
    let output = Command::new(agent.path()).arg("a").output().unwrap();

    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
    assert_eq!(agent.last_run().unwrap().args, vec!["a"]);
    drop(agent);
    let _ = std::fs::remove_dir_all(&tmp);
}
