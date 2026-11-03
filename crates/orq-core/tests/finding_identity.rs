use std::path::PathBuf;

use orq_core::review_input::{load_review_findings, InputError};

fn file(tag: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-finding-identity-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("review.json");
    std::fs::write(&path, text).unwrap();
    path
}

fn with_id(id: &str) -> String {
    format!(r#"{{"schema_version":1,"rounds":[{{"findings":[{{"id":"{id}","claim":"c"}}]}}]}}"#)
}

#[test]
fn an_id_longer_than_the_limit_is_refused_not_cut_into_a_colliding_key() {
    let long = "x".repeat(501);
    assert!(matches!(load_review_findings(&file("long", &with_id(&long))), Err(InputError::Malformed(_))));
}

#[test]
fn an_id_of_exactly_the_limit_is_kept_whole() {
    let exact = "y".repeat(500);
    let rounds = load_review_findings(&file("exact", &with_id(&exact))).unwrap();
    assert_eq!(rounds[0].findings[0].id, exact);
}

#[test]
fn an_id_with_control_characters_is_refused_so_two_ids_cannot_look_alike() {
    assert!(matches!(load_review_findings(&file("ctl", &with_id("a\\nb"))), Err(InputError::Malformed(_))));
    assert!(matches!(load_review_findings(&file("esc", &with_id("a\\u001b[2J"))), Err(InputError::Malformed(_))));
}
