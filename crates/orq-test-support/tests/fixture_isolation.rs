use orq_test_support::GitFixture;

#[test]
fn fixture_ignores_signing_in_the_inherited_global_git_config() {
    let dir = std::env::temp_dir().join(format!("orq-isolation-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("gitconfig");
    std::fs::write(&config, "[commit]\n\tgpgSign = true\n[gpg]\n\tprogram = /bin/false\n").unwrap();
    std::env::set_var("GIT_CONFIG_GLOBAL", &config);

    let fixture = GitFixture::new();

    assert!(fixture.root().join(".git").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
