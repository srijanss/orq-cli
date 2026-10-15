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
    let path = root.join("settings.toml");
    fs::create_dir(&path).unwrap();

    let error = Settings::load(&path).unwrap_err();

    assert!(matches!(error, SettingsError::Io { .. }), "{error:?}");
}

#[test]
fn a_parent_that_is_a_file_fails_the_save_and_creates_nothing() {
    let root = dir("parent-file");
    let blocker = root.join("orq");
    fs::write(&blocker, "not a directory").unwrap();
    let path = blocker.join("settings.toml");
    let error = match Settings::load(&path) {
        Err(error) => error,
        Ok(mut settings) => {
            settings.set("implementer", "harness", "claude");
            settings.save().unwrap_err()
        }
    };

    assert!(matches!(error, SettingsError::Io { .. }), "{error:?}");
    assert_eq!(fs::read_to_string(&blocker).unwrap(), "not a directory");
}

#[cfg(unix)]
#[test]
fn a_read_only_directory_fails_the_save_and_keeps_the_existing_file() {
    use std::os::unix::fs::PermissionsExt;
    let root = dir("read-only");
    let path = root.join("settings.toml");
    fs::write(&path, "[implementer]\nharness = \"a\"\nprofile = \"b\"\n").unwrap();
    let mut settings = Settings::load(&path).unwrap();
    settings.set("implementer", "harness", "changed");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();

    let result = settings.save();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();

    // Skip the assertion when running as a user that ignores permissions (root).
    if result.is_err() {
        assert!(matches!(result.unwrap_err(), SettingsError::Io { .. }));
        assert_eq!(fs::read_to_string(&path).unwrap(), "[implementer]\nharness = \"a\"\nprofile = \"b\"\n");
    }
}
