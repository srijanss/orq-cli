//! `await_delivery`: a feature a session of a run built becomes `awaiting_delivery` with `heldOn` the
//! run's branch and `heldBy` the run id; every other byte of the ledger is left as it was.

use std::path::PathBuf;

use orq_features::{await_delivery, deliver_run_features};

fn dir(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-await-delivery-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| name.to_string()).collect()
}

#[test]
fn a_held_on_branch_differing_inside_a_multibyte_character_is_replaced() {
    let path = dir("utf8").join(".tdd-features.json");
    let before = "[\n  {\n    \"featureName\": \"a\",\n    \"status\": \"pending\",\n    \"heldOn\": \"fix--é\",\n    \"heldBy\": \"run-1\"\n  }\n]\n";
    std::fs::write(&path, before).unwrap();

    let held = await_delivery(&path, &names(&["a"]), "fix--ê", "run-2").unwrap();

    assert_eq!(held, names(&["a"]));
    let after = before.replace("pending", "awaiting_delivery").replace("fix--é", "fix--ê").replace("run-1", "run-2");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), after);
}

#[test]
fn a_large_group_is_held_and_delivered_in_a_few_passes_over_the_ledger_keeping_each_entrys_layout() {
    let path = dir("large").join(".tdd-features.json");
    let count = 2000;
    let entries: Vec<String> = (0..count)
        .map(|i| if i % 2 == 0 { format!("  {{\n    \"featureName\": \"f{i}\",\n    \"status\": \"pending\"\n  }}") } else { format!("  {{\"featureName\": \"f{i}\", \"status\": \"pending\"}}") })
        .collect();
    std::fs::write(&path, format!("[\n{}\n]\n", entries.join(",\n"))).unwrap();
    let all: Vec<String> = (0..count).map(|i| format!("f{i}")).collect();

    let started = std::time::Instant::now();
    assert_eq!(await_delivery(&path, &all, "fix--f0", "run-1").unwrap(), all);
    assert_eq!(deliver_run_features(&path, &all, &[], "fix--f0", "2026-11-26T00:00:00Z").unwrap(), all);
    let took = started.elapsed();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("[\n  {\n    \"featureName\": \"f0\",\n    \"status\": \"completed\",\n    \"heldOn\": \"fix--f0\",\n    \"heldBy\": \"run-1\",\n    \"recordedAt\": \"2026-11-26T00:00:00Z\"\n  },\n"), "{}", &text[..300]);
    assert!(text.contains("\n  {\"featureName\": \"f1\", \"status\": \"completed\",\"heldOn\":\"fix--f0\",\"heldBy\":\"run-1\",\"recordedAt\":\"2026-11-26T00:00:00Z\"},\n"), "{}", &text[..600]);
    assert!(took < std::time::Duration::from_secs(3), "{count} features took {took:?}");
}
