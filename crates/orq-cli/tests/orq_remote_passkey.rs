//! `orq remote passkey list|remove`: managing the passkeys on this machine, which is also how
//! access is recovered when every device is lost.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_core::passkey_store::{Passkey, PasskeyStore};
use orq_core::storage::StorageRoots;

fn setup(tag: &str) -> (PathBuf, StorageRoots) {
    let root = orq_test_support::process_temp_dir().join(format!("orq-remote-passkey-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let roots = StorageRoots { data: root.join("data/orq"), config: root.join("config/orq"), state: root.join("state/orq"), cache: root.join("cache/orq") };
    roots.create_all().unwrap();
    (root, roots)
}

fn orq(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    command.args(args);
    for (name, dir) in [("XDG_STATE_HOME", "state"), ("XDG_DATA_HOME", "data"), ("XDG_CONFIG_HOME", "config"), ("XDG_CACHE_HOME", "cache"), ("HOME", "home")] {
        command.env(name, root.join(dir));
    }
    command.output().unwrap()
}

fn key(label: &str, id: &str) -> Passkey {
    Passkey { credential_id: id.into(), public_key: "PUBLICKEYMATERIAL".into(), sign_count: 0, label: label.into(), created_at: 1 }
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn list_shows_each_passkey_by_label_and_id_and_says_so_when_there_are_none() {
    let (root, roots) = setup("list");
    let empty = orq(&root, &["remote", "passkey", "list"]);
    assert!(empty.status.success());
    assert!(out(&empty).contains("no passkeys"), "{}", out(&empty));
    let store = PasskeyStore::open(&roots);
    store.add_first(key("phone", "AAAAAAAAAAAAAAAA")).unwrap();
    store.add(key("laptop", "BBBBBBBBBBBBBBBB")).unwrap();
    let listed = orq(&root, &["remote", "passkey", "list"]);
    let text = out(&listed);
    assert!(listed.status.success());
    assert!(text.contains("phone") && text.contains("AAAAAAAAAAAAAAAA") && text.contains("laptop") && text.contains("BBBBBBBBBBBBBBBB"), "{text}");
    assert!(!text.contains("PUBLICKEYMATERIAL"), "the public key is not shown: {text}");
}

#[test]
fn remove_deletes_one_passkey_by_label_and_refuses_an_unknown_label() {
    let (root, roots) = setup("remove");
    let store = PasskeyStore::open(&roots);
    store.add_first(key("phone", "AAAAAAAAAAAAAAAA")).unwrap();
    store.add(key("laptop", "BBBBBBBBBBBBBBBB")).unwrap();
    let removed = orq(&root, &["remote", "passkey", "remove", "phone"]);
    assert!(removed.status.success(), "{}", String::from_utf8_lossy(&removed.stderr));
    let labels: Vec<String> = store.list().unwrap().into_iter().map(|k| k.label).collect();
    assert_eq!(labels, ["laptop"]);
    let unknown = orq(&root, &["remote", "passkey", "remove", "tablet"]);
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("no passkey has that label"));
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn bad_arguments_are_a_usage_error_and_change_nothing() {
    let (root, roots) = setup("usage");
    PasskeyStore::open(&roots).add_first(key("phone", "AAAAAAAAAAAAAAAA")).unwrap();
    for args in [&["remote", "passkey"][..], &["remote", "passkey", "remove"], &["remote", "passkey", "remove", "a", "b"], &["remote", "passkey", "list", "extra"]] {
        let output = orq(&root, args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
    assert_eq!(PasskeyStore::open(&roots).list().unwrap().len(), 1);
}
