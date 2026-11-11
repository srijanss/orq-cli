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
    let base = std::env::temp_dir().join(format!("orq-settingsversion-{}-{tag}", std::process::id()));
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
