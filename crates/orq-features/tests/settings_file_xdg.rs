use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use orq_features::{settings_path, Settings, SettingsError};

fn dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-settings-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn xdg_config_home_wins_when_it_is_absolute() {
    let path = settings_path(Some(OsStr::new("/xdg")), Some(OsStr::new("/home/u")));

    assert_eq!(path, Some(PathBuf::from("/xdg/orq/settings.toml")));
}

#[test]
fn it_falls_back_to_home_dot_config_when_xdg_is_unset_empty_or_relative() {
    let expected = Some(PathBuf::from("/home/u/.config/orq/settings.toml"));

    assert_eq!(settings_path(None, Some(OsStr::new("/home/u"))), expected);
    assert_eq!(settings_path(Some(OsStr::new("")), Some(OsStr::new("/home/u"))), expected);
    assert_eq!(settings_path(Some(OsStr::new("relative/dir")), Some(OsStr::new("/home/u"))), expected);
}

#[test]
fn without_xdg_or_home_there_is_no_path() {
    assert_eq!(settings_path(None, None), None);
    assert_eq!(settings_path(None, Some(OsStr::new("relative"))), None);
}

#[test]
fn a_missing_file_loads_empty_and_is_not_created_by_loading() {
    let root = dir("missing");
    let path = root.join("orq").join("settings.toml");

    let settings = Settings::load(&path).unwrap();

    assert_eq!(settings.get("agents", "implementer"), None);
    assert!(!path.exists(), "load must not create the file or its directory");
    assert!(!root.join("orq").exists());
}

#[test]
fn the_first_save_creates_the_directory_and_a_private_file() {
    let root = dir("first-save");
    let path = root.join("orq").join("settings.toml");
    let mut settings = Settings::load(&path).unwrap();

    settings.set("agents", "implementer", "opus");
    settings.save().unwrap();

    let reloaded = Settings::load(&path).unwrap();
    assert_eq!(reloaded.get("agents", "implementer"), Some("opus".to_string()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[test]
fn unknown_keys_comments_and_tables_survive_a_save() {
    let root = dir("unknown");
    let path = root.join("settings.toml");
    let original = "# mine\n[future]\nflag = true\nlist = [1, 2]\n\n[agents]\nimplementer = \"opus\" # keep\nextra = \"x\"\n";
    fs::write(&path, original).unwrap();
    let mut settings = Settings::load(&path).unwrap();

    settings.set("agents", "reviewer", "gpt");
    settings.save().unwrap();

    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("# mine"), "{text}");
    assert!(text.contains("[future]\nflag = true\nlist = [1, 2]"), "{text}");
    assert!(text.contains("implementer = \"opus\" # keep"), "{text}");
    assert!(text.contains("extra = \"x\""), "{text}");
    assert!(text.contains("reviewer = \"gpt\""), "{text}");
}

#[test]
fn a_malformed_file_is_reported_and_left_byte_for_byte_alone() {
    let root = dir("malformed");
    let path = root.join("settings.toml");
    fs::write(&path, "[agents\nimplementer = ").unwrap();

    let error = Settings::load(&path).unwrap_err();

    assert!(matches!(error, SettingsError::Malformed { .. }), "{error:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "[agents\nimplementer = ");
}

#[test]
fn a_file_changed_since_load_is_a_conflict_and_nothing_is_written() {
    let root = dir("conflict");
    let path = root.join("settings.toml");
    fs::write(&path, "[agents]\nimplementer = \"a\"\n").unwrap();
    let mut settings = Settings::load(&path).unwrap();
    fs::write(&path, "[agents]\nimplementer = \"edited elsewhere\"\n").unwrap();

    settings.set("agents", "implementer", "b");
    let error = settings.save().unwrap_err();

    assert!(matches!(error, SettingsError::Conflict { .. }), "{error:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "[agents]\nimplementer = \"edited elsewhere\"\n");
}

#[test]
fn a_file_created_since_a_missing_load_is_a_conflict() {
    let root = dir("created");
    let path = root.join("settings.toml");
    let mut settings = Settings::load(&path).unwrap();
    fs::write(&path, "[agents]\nimplementer = \"other\"\n").unwrap();

    settings.set("agents", "implementer", "mine");

    assert!(matches!(settings.save().unwrap_err(), SettingsError::Conflict { .. }));
    assert_eq!(fs::read_to_string(&path).unwrap(), "[agents]\nimplementer = \"other\"\n");
}

#[test]
fn a_second_save_after_the_first_succeeds() {
    let root = dir("twice");
    let path: &Path = &root.join("settings.toml");
    let mut settings = Settings::load(path).unwrap();

    settings.set("agents", "implementer", "one");
    settings.save().unwrap();
    settings.set("agents", "implementer", "two");
    settings.save().unwrap();

    assert_eq!(Settings::load(path).unwrap().get("agents", "implementer"), Some("two".to_string()));
}
