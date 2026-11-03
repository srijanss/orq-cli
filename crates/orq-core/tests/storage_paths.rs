use std::collections::HashMap;
use std::path::PathBuf;

use orq_core::storage::StorageRoots;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    move |name| map.get(name).cloned()
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-storage-paths-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn roots_follow_the_xdg_variables_and_fall_back_to_home_defaults() {
    let roots = StorageRoots::resolve(&env(&[("HOME", "/home/u")])).unwrap();
    assert_eq!(roots.data, PathBuf::from("/home/u/.local/share/orq"));
    assert_eq!(roots.config, PathBuf::from("/home/u/.config/orq"));
    assert_eq!(roots.state, PathBuf::from("/home/u/.local/state/orq"));
    assert_eq!(roots.cache, PathBuf::from("/home/u/.cache/orq"));

    let roots = StorageRoots::resolve(&env(&[
        ("HOME", "/home/u"),
        ("XDG_DATA_HOME", "/x/data"),
        ("XDG_CONFIG_HOME", "/x/config"),
        ("XDG_STATE_HOME", "/x/state"),
        ("XDG_CACHE_HOME", "/x/cache"),
    ]))
    .unwrap();
    assert_eq!(roots.data, PathBuf::from("/x/data/orq"));
    assert_eq!(roots.config.join("config.toml"), roots.config_file());
    assert_eq!(roots.logs(), PathBuf::from("/x/state/orq/logs"));
    assert_eq!(roots.database(), PathBuf::from("/x/data/orq/orq.db"));
    assert_eq!(roots.runs(), PathBuf::from("/x/data/orq/runs"));
}

#[test]
fn relative_or_empty_xdg_values_are_ignored_and_a_missing_home_fails() {
    let roots = StorageRoots::resolve(&env(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "relative/dir"), ("XDG_CACHE_HOME", "")])).unwrap();
    assert_eq!(roots.data, PathBuf::from("/home/u/.local/share/orq"));
    assert_eq!(roots.cache, PathBuf::from("/home/u/.cache/orq"));
    assert!(StorageRoots::resolve(&env(&[])).is_err());
    assert!(StorageRoots::resolve(&env(&[("HOME", "not/absolute")])).is_err());
}

#[cfg(unix)]
#[test]
fn creating_the_roots_uses_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let base = scratch("create");
    let roots = StorageRoots::resolve(&env(&[("HOME", base.to_str().unwrap())])).unwrap();
    roots.create_all().unwrap();
    for dir in [&roots.data, &roots.config, &roots.state, &roots.cache, &roots.runs(), &roots.logs()] {
        let mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{} is {mode:o}", dir.display());
    }
    roots.create_all().unwrap(); // idempotent
}
