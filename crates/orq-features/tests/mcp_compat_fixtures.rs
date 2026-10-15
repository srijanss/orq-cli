//! Ledgers as the TDD MCP server writes them, and what Orq leaves of them.
use std::path::PathBuf;

use orq_features::{FeatureDraft, FieldEdits, LedgerStatus, LoadedLedger};
use serde_json::Value;

const MCP_LEDGER: &str = r#"[
  {
    "featureName": "done-one",
    "description": "Finished by a TDD cycle",
    "dependsOn": [],
    "status": "completed",
    "testFile": "-p orq-features --test done_one",
    "targetFiles": [],
    "recordedAt": "2026-10-01T09:30:00.000Z",
    "cyclesCompleted": 2
  },
  {
    "featureName": "working",
    "dependsOn": ["done-one"],
    "status": "in_progress",
    "testFile": "tests/working.rs",
    "targetFiles": ["src/working.rs"]
  },
  {
    "featureName": "future-status",
    "status": "blocked",
    "notes": {"owner": "mcp", "tags": ["a", 1]}
  },
  {
    "featureName": "plain",
    "description": "Waiting",
    "status": "draft"
  }
]
"#;

fn fixture(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-mcp-compat-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".tdd-features.json");
    std::fs::write(&path, MCP_LEDGER).unwrap();
    path
}

fn values(path: &PathBuf) -> Vec<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn mcp_produced_ledgers_load_with_unknown_statuses_kept_verbatim() {
    let loaded = LoadedLedger::load(&fixture("load")).unwrap();
    let entries = loaded.entries();

    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].cycles_completed, Some(2));
    assert_eq!(entries[0].recorded_at.as_deref(), Some("2026-10-01T09:30:00.000Z"));
    assert_eq!(entries[1].test_file.as_deref(), Some("tests/working.rs"));
    assert_eq!(entries[2].status, LedgerStatus::Unrecognised("blocked".into()));
}

#[test]
fn appending_keeps_every_existing_entry_and_writes_mcp_shaped_drafts() {
    let path = fixture("append");
    let before = values(&path);
    let loaded = LoadedLedger::load(&path).unwrap();
    let mut draft = FeatureDraft::new("fresh");
    draft.description = Some("From Orq".into());
    draft.depends_on = Some(vec!["done-one".into()]);
    draft.parallelizable = Some(true);

    loaded.append(&draft).unwrap();

    let after = values(&path);
    assert_eq!(&after[..4], &before[..]);
    let new = after[4].as_object().unwrap();
    assert_eq!(new["featureName"], "fresh");
    assert_eq!(new["status"], "draft");
    assert!(new["description"].is_string());
    assert!(new["dependsOn"].as_array().unwrap().iter().all(Value::is_string));
    assert!(new["parallelizable"].is_boolean());
    for ledger_owned in ["recordedAt", "cyclesCompleted"] {
        assert!(!new.contains_key(ledger_owned), "{ledger_owned} must never be invented");
    }
    for key in new.keys() {
        assert!(
            ["featureName", "description", "dependsOn", "status", "parallelizable"].contains(&key.as_str()),
            "unexpected property {key}"
        );
    }
}

#[test]
fn editing_one_feature_leaves_the_others_and_unknown_fields_untouched() {
    let path = fixture("edit");
    let before = values(&path);
    let loaded = LoadedLedger::load(&path).unwrap();
    let edits = FieldEdits { description: Some("Reworded".into()), ..Default::default() };

    loaded.edit_fields("plain", &edits).unwrap();

    let after = values(&path);
    assert_eq!(&after[..3], &before[..3]);
    assert_eq!(after[2]["notes"], before[2]["notes"]);
    assert_eq!(after[3]["description"], "Reworded");
    assert_eq!(after[3]["status"], "draft");
}

#[test]
fn promoting_changes_only_the_status_and_never_an_unknown_one() {
    let path = fixture("promote");
    let before = values(&path);
    let loaded = LoadedLedger::load(&path).unwrap();

    assert!(loaded.promote("future-status", None).is_err());
    assert_eq!(values(&path), before);

    loaded.promote("plain", None).unwrap();
    let after = values(&path);
    assert_eq!(&after[..3], &before[..3]);
    assert_eq!(after[3]["status"], "pending");
    assert_eq!(after[3]["description"], "Waiting");
}
