//! The registered passkeys: a private file in the state directory that is empty by default, and
//! where a missing, unreadable or doubtful file refuses everyone.

use orq_core::passkey_store::{Passkey, PasskeyStore};
use orq_core::storage::StorageRoots;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-passkeys-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") }
}

/// Directories only this user can enter or change, as the store requires all the way up.
fn private_dirs(path: &std::path::Path) {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(path).unwrap();
}

fn key(id: &str, label: &str) -> Passkey {
    Passkey { credential_id: id.into(), public_key: "pQECAyYgASFYIA".into(), sign_count: 0, label: label.into(), created_at: 1_700_000_000 }
}

fn mode(path: &std::path::Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn with_no_file_the_store_is_empty_and_refuses_everyone_and_reading_creates_nothing() {
    let roots = roots("missing");
    let store = PasskeyStore::open(&roots);
    assert_eq!(store.list().unwrap(), vec![]);
    assert!(!store.any_registered());
    assert!(!roots.state.exists(), "reading leaves no file or directory behind");
}

#[test]
fn an_added_passkey_is_kept_in_a_private_file_that_holds_only_public_facts() {
    let roots = roots("add");
    let store = PasskeyStore::open(&roots);
    store.add(key("cred-1", "phone")).unwrap();
    store.add(key("cred-2", "laptop")).unwrap();
    assert_eq!(store.list().unwrap(), vec![key("cred-1", "phone"), key("cred-2", "laptop")]);
    assert!(store.any_registered());
    assert_eq!(store.find("cred-2").unwrap().unwrap().label, "laptop");
    assert_eq!(store.find("nope").unwrap(), None);
    let path = PasskeyStore::path(&roots);
    assert_eq!((mode(&path), mode(&roots.state)), (0o600, 0o700));
    let text = fs::read_to_string(&path).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    let fields: Vec<&String> = doc["passkeys"][0].as_object().unwrap().keys().collect();
    assert_eq!(fields, ["created_at", "credential_id", "label", "public_key", "sign_count"], "{text}");
    for banned in ["private", "secret", "token"] {
        assert!(!text.to_lowercase().contains(banned), "{text}");
    }
    let names: Vec<_> = fs::read_dir(&roots.state).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    assert!(names.iter().all(|n| n == "passkeys.json" || n == ".passkeys.lock"), "no temporary files remain: {names:?}");
    assert_eq!(PasskeyStore::open(&roots).list().unwrap().len(), 2, "a new handle reads the same file");
}

fn write(roots: &StorageRoots, text: &str) -> std::path::PathBuf {
    private_dirs(&roots.state);
    let path = PasskeyStore::path(roots);
    fs::write(&path, text).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}

fn good() -> String {
    r#"{"version":1,"passkeys":[{"credential_id":"cred-1","public_key":"AQID","sign_count":3,"label":"phone","created_at":5}]}"#.into()
}

#[test]
fn a_well_formed_file_is_read_back() {
    let roots = roots("good");
    write(&roots, &good());
    let store = PasskeyStore::open(&roots);
    assert_eq!(store.list().unwrap(), vec![Passkey { credential_id: "cred-1".into(), public_key: "AQID".into(), sign_count: 3, label: "phone".into(), created_at: 5 }]);
    assert!(store.any_registered());
}

#[test]
fn an_unreadable_or_doubtful_file_is_an_error_and_refuses_everyone() {
    let bad: Vec<(&str, String)> = vec![
        ("empty file", String::new()),
        ("not json", "passkeys".into()),
        ("not an object", "[]".into()),
        ("wrong version", good().replace("\"version\":1", "\"version\":2")),
        ("no version", good().replace("\"version\":1,", "")),
        ("passkeys not a list", r#"{"version":1,"passkeys":{}}"#.into()),
        ("missing field", good().replace("\"label\":\"phone\",", "")),
        ("extra field", good().replace("\"label\"", "\"private_key\":\"x\",\"label\"")),
        ("count not a number", good().replace("\"sign_count\":3", "\"sign_count\":\"3\"")),
        ("count negative", good().replace("\"sign_count\":3", "\"sign_count\":-1")),
        ("count too big", good().replace("\"sign_count\":3", "\"sign_count\":4294967296")),
        ("id not base64url", good().replace("cred-1", "cred 1!")),
        ("empty id", good().replace("cred-1", "")),
        ("key not base64url", good().replace("AQID", "a b")),
        ("empty label", good().replace("phone", "")),
        ("control in label", good().replace("phone", "ph\\u0007one")),
        ("long label", good().replace("phone", &"x".repeat(65))),
        ("duplicate id", good().replace("}]}", "},{\"credential_id\":\"cred-1\",\"public_key\":\"AQID\",\"sign_count\":0,\"label\":\"two\",\"created_at\":6}]}")),
        ("duplicate label", good().replace("}]}", "},{\"credential_id\":\"cred-2\",\"public_key\":\"AQID\",\"sign_count\":0,\"label\":\"phone\",\"created_at\":6}]}")),
        ("too big", format!("{}{}", good(), " ".repeat(1_100_000))),
    ];
    for (name, text) in bad {
        let roots = roots("bad");
        write(&roots, &text);
        let store = PasskeyStore::open(&roots);
        assert!(store.list().is_err(), "{name}");
        assert!(!store.any_registered(), "{name}: refuses everyone");
        assert!(store.find("cred-1").is_err(), "{name}");
        assert!(store.add(key("cred-9", "other")).is_err(), "{name}: nothing is added to a file that cannot be trusted");
        assert_eq!(fs::read_to_string(PasskeyStore::path(&roots)).unwrap(), text, "{name}: the file is left as it was");
    }
}

#[test]
fn a_file_others_can_reach_or_that_is_not_a_plain_file_is_refused() {
    for loose in [0o644, 0o640, 0o604, 0o660] {
        let roots = roots("loose");
        let path = write(&roots, &good());
        fs::set_permissions(&path, fs::Permissions::from_mode(loose)).unwrap();
        let store = PasskeyStore::open(&roots);
        assert!(store.list().is_err() && !store.any_registered(), "{loose:o}");
    }
    let roots1 = roots("link");
    let path = write(&roots1, &good());
    let real = roots1.state.join("real.json");
    fs::rename(&path, &real).unwrap();
    symlink(&real, &path).unwrap();
    assert!(!PasskeyStore::open(&roots1).any_registered(), "a link is not followed");
    let roots2 = roots("dir");
    private_dirs(&PasskeyStore::path(&roots2));
    let store = PasskeyStore::open(&roots2);
    assert!(store.list().is_err() && !store.any_registered(), "a directory");
}

#[test]
fn a_duplicate_credential_or_label_or_a_bad_value_is_refused_and_changes_nothing() {
    let roots = roots("dup");
    let store = PasskeyStore::open(&roots);
    store.add(key("cred-1", "phone")).unwrap();
    let before = fs::read_to_string(PasskeyStore::path(&roots)).unwrap();
    let mut bad_key = key("cred-3", "x");
    bad_key.public_key = "not valid!".into();
    for refused in [key("cred-1", "other"), key("cred-2", "phone"), key("", "x"), key("cred-3", ""), key("cred-3", &"x".repeat(65)), key("cred 3", "x"), bad_key] {
        assert!(store.add(refused.clone()).is_err(), "{refused:?}");
    }
    assert_eq!(fs::read_to_string(PasskeyStore::path(&roots)).unwrap(), before);
}

#[test]
fn passkeys_added_at_the_same_moment_are_all_kept() {
    let roots = std::sync::Arc::new(roots("race"));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
    let handles: Vec<_> = (0..12)
        .map(|n| {
            let (roots, barrier) = (roots.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                PasskeyStore::open(&roots).add(key(&format!("cred-{n}"), &format!("device-{n}")))
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap().expect("every add succeeds");
    }
    assert_eq!(PasskeyStore::open(&roots).list().unwrap().len(), 12);
}

#[test]
fn removing_by_label_cuts_that_passkey_off_and_removing_the_last_leaves_an_empty_valid_store() {
    let roots = roots("remove");
    let store = PasskeyStore::open(&roots);
    store.add(key("cred-1", "phone")).unwrap();
    store.add(key("cred-2", "laptop")).unwrap();
    assert!(store.remove("tablet").is_err(), "an unknown label is an error");
    assert_eq!(store.list().unwrap().len(), 2);
    store.remove("phone").unwrap();
    assert_eq!(store.list().unwrap(), vec![key("cred-2", "laptop")]);
    store.remove("laptop").unwrap();
    assert_eq!(store.list().unwrap(), vec![]);
    assert!(!store.any_registered());
    assert_eq!(mode(&PasskeyStore::path(&roots)), 0o600);
    store.add(key("cred-3", "phone")).unwrap();
    assert!(store.any_registered(), "a passkey can be added again after the store was emptied");
}

#[test]
fn a_state_directory_that_is_a_link_or_open_to_others_is_refused() {
    let roots = roots("dirs");
    let real = roots.state.with_file_name("elsewhere");
    private_dirs(&real);
    symlink(&real, &roots.state).unwrap();
    let store = PasskeyStore::open(&roots);
    assert!(store.list().is_err() && store.add(key("a", "one")).is_err() && !store.any_registered());
    assert!(fs::read_dir(&real).unwrap().next().is_none(), "nothing is written through the link");
    fs::remove_file(&roots.state).unwrap();
    for loose in [0o770, 0o707, 0o777] {
        private_dirs(&roots.state);
        fs::set_permissions(&roots.state, fs::Permissions::from_mode(loose)).unwrap();
        assert!(store.list().is_err() && store.add(key("a", "one")).is_err(), "{loose:o}");
        assert!(!store.any_registered());
        fs::remove_dir_all(&roots.state).unwrap();
    }
}

#[test]
fn a_store_cannot_grow_past_the_size_it_can_be_read_back_at() {
    let roots = roots("limit");
    let store = PasskeyStore::open(&roots);
    let big = |n: usize| Passkey { credential_id: format!("id{n}"), public_key: "A".repeat(4096), sign_count: 0, label: format!("label-{n}"), created_at: 1 };
    let mut added = 0;
    for n in 0..400 {
        if store.add(big(n)).is_err() {
            break;
        }
        added += 1;
    }
    assert!(added > 10 && added < 400, "the limit was reached after {added}");
    let path = PasskeyStore::path(&roots);
    assert!(fs::metadata(&path).unwrap().len() <= 1 << 20);
    assert_eq!(store.list().unwrap().len(), added, "the store is still readable and unchanged by the refused add");
    assert!(store.any_registered());
    let names: Vec<_> = fs::read_dir(&roots.state).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    assert!(names.iter().all(|n| n == "passkeys.json" || n == ".passkeys.lock"), "{names:?}");
}

#[test]
fn a_write_that_fails_leaves_the_store_as_it_was_and_the_lock_free() {
    let roots = roots("fail");
    let store = PasskeyStore::open(&roots);
    store.add(key("a", "one")).unwrap();
    let before = fs::read_to_string(PasskeyStore::path(&roots)).unwrap();
    // A state directory that cannot be written to: creating the temporary file fails.
    fs::set_permissions(&roots.state, fs::Permissions::from_mode(0o500)).unwrap();
    let denied = store.add(key("b", "two"));
    fs::set_permissions(&roots.state, fs::Permissions::from_mode(0o700)).unwrap();
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    assert!(denied.is_err());
    assert_eq!(fs::read_to_string(PasskeyStore::path(&roots)).unwrap(), before);
    let names: Vec<_> = fs::read_dir(&roots.state).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    assert!(names.iter().all(|n| n == "passkeys.json" || n == ".passkeys.lock"), "{names:?}");
    store.add(key("b", "two")).expect("the lock was released, so a later change goes through");
    assert_eq!(store.list().unwrap().len(), 2);
}

#[test]
fn an_unwritable_lock_file_refuses_the_change() {
    let roots = roots("lock");
    let store = PasskeyStore::open(&roots);
    store.add(key("a", "one")).unwrap();
    let before = fs::read_to_string(PasskeyStore::path(&roots)).unwrap();
    let lock = roots.state.join(".passkeys.lock");
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();
    assert!(store.add(key("b", "two")).is_err());
    assert_eq!(fs::read_to_string(PasskeyStore::path(&roots)).unwrap(), before);
}
