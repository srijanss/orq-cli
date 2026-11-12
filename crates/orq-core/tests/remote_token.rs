use std::os::unix::fs::{symlink, PermissionsExt};

use orq_core::remote_inbox::{handle, RemoteConfig, Request};
use orq_core::storage::StorageRoots;

const GOOD: &str = "0123456789abcdef0123456789abcdef0123";

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-remote-token-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn write(roots: &StorageRoots, text: &str, mode: u32) {
    let path = RemoteConfig::token_path(roots);
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn a_private_token_file_is_loaded_and_trimmed() {
    let roots = roots("ok");
    write(&roots, &format!("{GOOD}\n"), 0o600);
    assert_eq!(RemoteConfig::load(&roots).unwrap().token, GOOD);
}

#[test]
fn a_missing_group_readable_short_empty_or_linked_token_file_is_refused_without_echoing_it() {
    let roots = roots("bad");
    assert!(RemoteConfig::load(&roots).is_err(), "missing");
    write(&roots, GOOD, 0o640);
    let why = RemoteConfig::load(&roots).err().unwrap();
    assert!(!why.contains(GOOD), "{why}");
    write(&roots, "SENTINELSHORT", 0o600);
    let why = RemoteConfig::load(&roots).err().unwrap();
    assert!(!why.contains("SENTINELSHORT"), "{why}");
    write(&roots, &format!("{GOOD} SENTINELSPACE"), 0o600);
    let why = RemoteConfig::load(&roots).err().unwrap();
    assert!(!why.contains("SENTINELSPACE"), "{why}");
    write(&roots, &format!("{GOOD}\u{7}"), 0o600);
    assert!(RemoteConfig::load(&roots).is_err(), "control");
    let path = RemoteConfig::token_path(&roots);
    std::fs::write(&path, [0xffu8; 40]).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(RemoteConfig::load(&roots).is_err(), "not utf-8");
    write(&roots, "  \n", 0o600);
    assert!(RemoteConfig::load(&roots).is_err(), "empty");

    let real = roots.config.join("elsewhere");
    std::fs::write(&real, GOOD).unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
    let path = RemoteConfig::token_path(&roots);
    std::fs::remove_file(&path).unwrap();
    symlink(&real, &path).unwrap();
    assert!(RemoteConfig::load(&roots).is_err(), "symlink");
}

#[test]
fn a_stranger_and_a_wrong_token_get_exactly_the_same_refusal() {
    let roots = roots("same");
    let config = RemoteConfig { token: GOOD.into() };
    let ask = |peer: &str, auth: Option<&str>| {
        let r = handle(&roots, &config, "p", "n", &Request { method: "GET", path: "/api/reviews", peer: peer.parse().unwrap(), authorization: auth });
        (r.status, r.body)
    };
    let good = format!("Bearer {GOOD}");
    let stranger = ask("8.8.8.8", Some(&good));
    let wrong = ask("127.0.0.1", Some("Bearer nope"));
    let none = ask("127.0.0.1", None);
    assert_eq!(stranger, wrong);
    assert_eq!(wrong, none);
    assert_eq!(stranger.0, 401);
}

#[test]
fn an_oversized_token_file_is_refused_not_cut_short() {
    let roots = roots("big");
    write(&roots, &format!("{GOOD}{}", "x".repeat(5000)), 0o600);
    assert!(RemoteConfig::load(&roots).is_err());
    // Exactly at the limit is a long token, accepted.
    write(&roots, &"x".repeat(4096), 0o600);
    assert_eq!(RemoteConfig::load(&roots).unwrap().token.len(), 4096);
}

#[test]
fn the_metadata_rules_refuse_a_foreign_owner_group_access_and_non_files() {
    use orq_core::remote_inbox::check_token_file_meta as check;
    assert!(check(true, 1000, 0o100600, 1000).is_ok());
    assert!(check(true, 0, 0o100600, 1000).is_err(), "other owner");
    assert!(check(true, 1000, 0o100640, 1000).is_err(), "group readable");
    assert!(check(true, 1000, 0o100604, 1000).is_err(), "world readable");
    assert!(check(false, 1000, 0o040600, 1000).is_err(), "not a file");
}
