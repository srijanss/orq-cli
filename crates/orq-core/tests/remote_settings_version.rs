//! GET /api/settings carries a version of config.toml, and a settings change is written only when the
//! version it presents is still the file's: otherwise 409 "Settings changed since you opened them" and
//! nothing is written.

use orq_core::remote_inbox::{handle_with, public_settings, ProjectsProvider, RemoteConfig, Request};
use orq_core::remote_settings::write_if_version;
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

struct Fake {
    config: std::path::PathBuf,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn settings(&self) -> Result<Option<Value>, String> {
        public_settings(&self.config).map(Some)
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-settingsversion-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn version(roots: &StorageRoots) -> String {
    let request = Request { method: "GET", path: "/api/settings", peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let reply = handle_with(roots, &RemoteConfig { token: "tok".into() }, "p", "p", &request, &Fake { config: roots.config_file() });
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.body["settings"]["version"].as_str().expect("a version").to_string()
}

#[test]
fn a_change_is_written_only_while_the_version_it_was_read_at_is_still_current() {
    let roots = roots("write");
    std::fs::write(roots.config_file(), "# kept\nremote_actions = false\n").unwrap();
    let read = version(&roots);
    assert_eq!(read.len(), 64, "{read}");

    write_if_version(&roots, &read, |doc| doc["remote_actions"] = toml_edit::value(true)).expect("written");
    let text = std::fs::read_to_string(roots.config_file()).unwrap();
    assert!(text.contains("# kept") && text.contains("remote_actions = true"), "{text}");
    let after = version(&roots);
    assert_ne!(after, read);

    // The version read before that change is stale now: nothing is written.
    let refused = write_if_version(&roots, &read, |doc| doc["remote_actions"] = toml_edit::value(false)).expect_err("stale");
    assert_eq!(refused.status(), 409);
    assert_eq!(refused.to_string(), "Settings changed since you opened them");
    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), text);
    assert_eq!(version(&roots), after);
}

#[test]
fn a_writer_waits_for_the_lock_and_then_refuses_a_change_made_while_it_waited() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let roots = roots("lock");
    std::fs::write(roots.config_file(), "remote_actions = false\n").unwrap();
    let read = version(&roots);

    // Another Orq writer holds the lock and changes the file while this write waits for it.
    let held = std::fs::File::open(&roots.config).unwrap();
    held.lock().unwrap();
    let edited = Arc::new(AtomicBool::new(false));
    let (started, began) = std::sync::mpsc::channel();
    let writer = {
        let (roots, edited) = (roots.clone(), edited.clone());
        std::thread::spawn(move || {
            started.send(()).unwrap();
            write_if_version(&roots, &read, |doc| {
                edited.store(true, Ordering::SeqCst);
                doc["remote_actions"] = toml_edit::value(true);
            })
        })
    };
    began.recv().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!writer.is_finished(), "the writer must wait while the lock is held");
    assert!(!edited.load(Ordering::SeqCst), "nothing is edited while the lock is held");
    std::fs::write(roots.config_file(), "# theirs\nremote_actions = false\n").unwrap();
    held.unlock().unwrap();

    let refused = writer.join().unwrap().expect_err("the file changed while it waited");
    assert_eq!(refused.status(), 409);
    assert!(!edited.load(Ordering::SeqCst), "a refused change is never edited");
    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), "# theirs\nremote_actions = false\n");
}

#[test]
fn of_writers_racing_from_one_version_exactly_one_wins() {
    let roots = roots("race");
    std::fs::write(roots.config_file(), "remote_actions = false\n").unwrap();
    let read = version(&roots);
    let gate = std::sync::Arc::new(std::sync::Barrier::new(8));
    let writers: Vec<_> = (0..8)
        .map(|n| {
            let (roots, read, gate) = (roots.clone(), read.clone(), gate.clone());
            std::thread::spawn(move || {
                gate.wait();
                write_if_version(&roots, &read, |doc| doc["writer"] = toml_edit::value(n as i64)).map(|()| n)
            })
        })
        .collect();
    let results: Vec<_> = writers.into_iter().map(|w| w.join().unwrap()).collect();
    let winners: Vec<i64> = results.iter().filter_map(|r| r.as_ref().ok().copied()).collect();
    assert_eq!(winners.len(), 1, "{results:?}");
    assert!(results.iter().filter_map(|r| r.as_ref().err()).all(|e| e.status() == 409), "{results:?}");
    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), format!("remote_actions = false\nwriter = {}\n", winners[0]));
}

#[test]
fn an_absent_file_is_created_and_a_broken_one_is_left_alone() {
    let roots = roots("absent");
    let read = version(&roots);
    write_if_version(&roots, &read, |doc| doc["remote_actions"] = toml_edit::value(true)).expect("created");
    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), "remote_actions = true\n");

    let broken = "this is [not toml";
    std::fs::write(roots.config_file(), broken).unwrap();
    let read = orq_core::remote_settings::version_of(broken);
    let failed = write_if_version(&roots, &read, |doc| doc["remote_actions"] = toml_edit::value(false)).expect_err("broken");
    assert_eq!(failed.status(), 500);
    assert!(!failed.to_string().contains(roots.config.to_str().unwrap()), "{failed}");
    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), broken);
}

#[test]
fn the_version_is_keyed_so_it_is_no_offline_check_on_the_file_text() {
    use sha2::{Digest, Sha256};
    let roots = roots("keyed");
    let text = "[implementer]\nharness = \"claude\"\ntoken = \"hidden\"\n";
    std::fs::write(roots.config_file(), text).unwrap();
    let read = version(&roots);
    let plain: String = Sha256::digest(text.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
    assert_ne!(read, plain, "the version must not be the bare hash of the file");
    assert_eq!(read, version(&roots), "the same text keeps the same version");
}
