use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;

use orq_test_support::{FakeAgent, GitFixture};

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn temp_directories_are_private_and_never_adopt_pre_created_ones() {
    let tmp = std::env::temp_dir().join(format!("orq-private-{}", std::process::id()));
    // Atomic and private: fails if the path already exists, so a pre-created
    // directory or symlink is never adopted.
    std::fs::DirBuilder::new().mode(0o700).create(&tmp).unwrap();
    std::env::set_var("TMPDIR", &tmp);
    // Names an attacker could predict from the pid and a small counter.
    let pid = std::process::id();
    let mut planted = Vec::new();
    for n in 0..4 {
        for prefix in ["orq-fake-agent", "orq-fixture"] {
            let dir = tmp.join(format!("{prefix}-{pid}-{n}"));
            std::fs::create_dir_all(&dir).unwrap();
            planted.push(dir);
        }
    }

    let agent = FakeAgent::builder().build();
    let fixture = GitFixture::new();

    let agent_dir = agent.path().parent().unwrap().to_path_buf();
    let fixture_base = fixture.root().parent().unwrap().to_path_buf();
    assert!(!planted.contains(&agent_dir), "adopted {agent_dir:?}");
    assert!(!planted.contains(&fixture_base), "adopted {fixture_base:?}");
    assert_eq!(mode(&agent_dir), 0o700);
    assert_eq!(mode(&fixture_base), 0o700);

    drop((agent, fixture));
    let _ = std::fs::remove_dir_all(&tmp);
}
