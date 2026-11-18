use std::collections::HashMap;
use std::path::PathBuf;

use orq_core::storage::StorageRoots;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    move |name| map.get(name).cloned()
}

fn scratch(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-storage-paths-{}-{tag}", std::process::id()));
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
fn under_the_test_sandbox_a_folder_outside_the_temp_dir_is_refused_by_name() {
    let error = StorageRoots::resolve(&env(&[("ORQ_TEST_SANDBOX", "1"), ("TMPDIR", "/t"), ("HOME", "/home/u"), ("XDG_STATE_HOME", "/t/state")])).unwrap_err();
    assert!(error.to_string().contains("/home/u/.local/share/orq"), "{error}");
    assert!(error.to_string().contains("ORQ_TEST_SANDBOX"), "{error}");

    // Without TMPDIR the temp dir is /tmp, and every folder inside it is accepted.
    let roots = StorageRoots::resolve(&env(&[("ORQ_TEST_SANDBOX", "1"), ("HOME", "/tmp/home")])).unwrap();
    assert_eq!(roots.data, PathBuf::from("/tmp/home/.local/share/orq"));

    // A path that climbs out of the temp dir is not inside it.
    let error = StorageRoots::resolve(&env(&[("ORQ_TEST_SANDBOX", "1"), ("HOME", "/tmp/home"), ("XDG_CONFIG_HOME", "/tmp/../home/u/.config")])).unwrap_err();
    assert!(error.to_string().contains("/tmp/../home/u/.config/orq"), "{error}");
}

#[cfg(unix)]
#[test]
fn under_the_test_sandbox_a_symlink_out_of_the_temp_dir_is_refused() {
    let base = scratch("sandbox-symlink");
    let temp = base.join("tmp");
    let protected = base.join("real-home");
    std::fs::create_dir_all(&temp).unwrap();
    std::fs::create_dir_all(&protected).unwrap();
    std::os::unix::fs::symlink(&protected, temp.join("home")).unwrap();
    let home = temp.join("home");

    // The data folder does not exist yet: the existing ancestor (the link) decides.
    let error = StorageRoots::resolve(&env(&[("ORQ_TEST_SANDBOX", "1"), ("TMPDIR", temp.to_str().unwrap()), ("HOME", home.to_str().unwrap())])).unwrap_err();
    assert!(error.to_string().contains(&home.join(".local/share/orq").display().to_string()), "{error}");

    // A plain directory inside the same temp dir is still accepted.
    let plain = temp.join("plain-home");
    std::fs::create_dir_all(&plain).unwrap();
    StorageRoots::resolve(&env(&[("ORQ_TEST_SANDBOX", "1"), ("TMPDIR", temp.to_str().unwrap()), ("HOME", plain.to_str().unwrap())])).unwrap();
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

#[cfg(unix)]
#[test]
fn directories_that_already_exist_with_loose_permissions_are_tightened() {
    use std::os::unix::fs::PermissionsExt;
    let base = scratch("tighten");
    let roots = StorageRoots::resolve(&env(&[("HOME", base.to_str().unwrap())])).unwrap();
    std::fs::create_dir_all(roots.runs()).unwrap();
    std::fs::set_permissions(&roots.data, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(roots.runs(), std::fs::Permissions::from_mode(0o777)).unwrap();
    roots.create_all().unwrap();
    for dir in [&roots.data, &roots.runs()] {
        assert_eq!(std::fs::metadata(dir).unwrap().permissions().mode() & 0o777, 0o700, "{}", dir.display());
    }
}

#[cfg(unix)]
#[test]
fn refusing_a_symlinked_directory_never_changes_what_it_points_at() {
    use std::os::unix::fs::PermissionsExt;
    let base = scratch("victim");
    let victim = scratch("victim-target");
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let roots = StorageRoots::resolve(&env(&[("HOME", base.to_str().unwrap())])).unwrap();
    std::fs::create_dir_all(roots.data.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&victim, &roots.data).unwrap();
    assert!(roots.create_all().is_err());
    assert_eq!(std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777, 0o755, "the link's target must keep its mode");
}

#[cfg(unix)]
#[test]
fn a_symlinked_orq_directory_is_refused_instead_of_followed() {
    let base = scratch("symlinked");
    let elsewhere = scratch("symlinked-elsewhere");
    let roots = StorageRoots::resolve(&env(&[("HOME", base.to_str().unwrap())])).unwrap();
    std::fs::create_dir_all(roots.data.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &roots.data).unwrap();
    assert!(roots.create_all().is_err());
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0, "nothing may be created behind the link");

    // A symlinked subdirectory inside a real root is refused too.
    let base = scratch("symlinked-sub");
    let roots = StorageRoots::resolve(&env(&[("HOME", base.to_str().unwrap())])).unwrap();
    std::fs::create_dir_all(&roots.data).unwrap();
    std::os::unix::fs::symlink(&elsewhere, roots.runs()).unwrap();
    assert!(roots.create_all().is_err());
}
