use std::fs;

use orq_features::{Settings, SettingsError};

fn dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-settings-io-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_path_that_is_a_directory_fails_to_load_with_io() {
    let root = dir("is-dir");
    let path = root.join("config.toml");
    fs::create_dir(&path).unwrap();

    let error = Settings::load(&path).unwrap_err();

    assert!(matches!(error, SettingsError::Io { .. }), "{error:?}");
}

#[test]
fn a_parent_replaced_by_a_file_after_load_fails_the_save() {
    let root = dir("parent-file");
    let parent = root.join("orq");
    fs::create_dir(&parent).unwrap();
    let path = parent.join("config.toml");
    let mut settings = Settings::load(&path).unwrap();
    settings.set("implementer", "harness", "claude");
    fs::remove_dir(&parent).unwrap();
    fs::write(&parent, "not a directory").unwrap();

    let error = settings.save().unwrap_err();

    assert!(matches!(error, SettingsError::Io { .. }), "{error:?}");
    assert_eq!(fs::read_to_string(&parent).unwrap(), "not a directory");
}

#[cfg(unix)]
#[test]
fn a_read_only_directory_fails_the_save_and_keeps_the_existing_file() {
    use std::os::unix::fs::PermissionsExt;
    let root = dir("read-only");
    let path = root.join("config.toml");
    let original = "[implementer]\nharness = \"a\"\nprofile = \"b\"\n";
    fs::write(&path, original).unwrap();
    let mut settings = Settings::load(&path).unwrap();
    settings.set("implementer", "harness", "changed");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();
    // Privileged users ignore directory permissions; detect that explicitly.
    let bypassed = fs::write(root.join("probe"), "x").is_ok();

    let result = settings.save();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();

    if bypassed {
        eprintln!("skipped: permissions are not enforced for this user");
        return;
    }
    assert!(matches!(result.unwrap_err(), SettingsError::Io { .. }));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
}
