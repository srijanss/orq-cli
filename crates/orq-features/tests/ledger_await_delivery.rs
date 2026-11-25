//! `await_delivery`: a feature a session of a run built becomes `awaiting_delivery` with `heldOn` the
//! run's branch and `heldBy` the run id; every other byte of the ledger is left as it was.

use std::path::PathBuf;

use orq_features::await_delivery;

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
