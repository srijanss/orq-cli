//! The Todo list in orq.db: items added, edited with the version read, and deleted; limits said in words.

use std::path::PathBuf;

use orq_core::store::todo::TodoChange;
use orq_core::store::Store;

fn scratch(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-todo-store-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("orq.db")
}

fn added(change: TodoChange) -> orq_core::store::todo::TodoItem {
    match change {
        TodoChange::Done(item) => item,
        other => panic!("not added: {other:?}"),
    }
}

#[test]
fn items_are_listed_newest_first_and_kept_across_opens() {
    let db = scratch("list");
    let store = Store::open(&db).unwrap();
    assert!(store.todos().unwrap().is_empty());
    let first = added(store.todo_add("one", "first", 100).unwrap());
    added(store.todo_add("two", "", 100).unwrap());
    drop(store);
    let store = Store::open(&db).unwrap();
    let items = store.todos().unwrap();
    assert_eq!(items.iter().map(|item| item.name.as_str()).collect::<Vec<_>>(), ["two", "one"]);
    assert_eq!(items[1], first);
    assert_eq!((first.created_at, first.updated_at, first.description.as_str()), (100, 100, "first"));
}

#[test]
fn an_edit_needs_the_version_read_and_limits_are_said_in_words() {
    let db = scratch("edit");
    let store = Store::open(&db).unwrap();
    let item = added(store.todo_add("  one  ", "", 100).unwrap());
    assert_eq!(item.name, "one", "the name is trimmed");
    let edited = added(store.todo_edit(item.id, "one, better", "why", item.version, 200).unwrap());
    assert_eq!((edited.name.as_str(), edited.description.as_str(), edited.created_at, edited.updated_at), ("one, better", "why", 100, 200));
    assert!(edited.version > item.version);
    assert_eq!(store.todo_edit(item.id, "stale", "", item.version, 300).unwrap(), TodoChange::Conflict);
    assert_eq!(store.todo_edit(item.id + 99, "x", "", 1, 300).unwrap(), TodoChange::Unknown);
    assert_eq!(store.todo(item.id).unwrap(), Some(edited));

    let invalid = |why: &str| TodoChange::Invalid(why.to_string());
    assert_eq!(store.todo_add("  ", "", 1).unwrap(), invalid("the name is empty"));
    assert_eq!(store.todo_add(&"n".repeat(201), "", 1).unwrap(), invalid("the name is longer than 200 characters"));
    added(store.todo_add(&"\u{e9}".repeat(200), &"d".repeat(20_000), 1).unwrap());
    assert_eq!(store.todo_add("n", &"d".repeat(20_001), 1).unwrap(), invalid("the description is longer than 20000 characters"));
    assert_eq!(store.todo_edit(item.id, "n", &"d".repeat(20_001), 2, 1).unwrap(), invalid("the description is longer than 20000 characters"));

    assert!(store.todo_delete(item.id).unwrap());
    assert!(!store.todo_delete(item.id).unwrap());
    assert_eq!(store.todo(item.id).unwrap(), None);
}

#[test]
fn a_wish_to_queue_a_draft_once_approved_is_kept_per_project_until_dropped() {
    let db = scratch("wish");
    let store = Store::open(&db).unwrap();
    let alpha = db.parent().unwrap().join("alpha");
    let beta = db.parent().unwrap().join("beta");
    assert!(!store.queue_wish(&alpha, "f-one").unwrap());
    store.queue_wish_set(&alpha, "f-one", 10).unwrap();
    store.queue_wish_set(&alpha, "f-one", 11).unwrap();
    assert!(store.queue_wish(&alpha, "f-one").unwrap());
    assert!(!store.queue_wish(&beta, "f-one").unwrap(), "a wish belongs to its project");
    assert!(store.queue_wish_drop(&alpha, "f-one").unwrap());
    assert!(!store.queue_wish_drop(&alpha, "f-one").unwrap(), "dropped once");
    assert!(!store.queue_wish(&alpha, "f-one").unwrap());
}

#[test]
fn the_list_holds_at_most_500_items() {
    let db = scratch("full");
    let store = Store::open(&db).unwrap();
    for n in 0..500 {
        added(store.todo_add(&format!("item {n}"), "", n).unwrap());
    }
    assert_eq!(store.todo_add("one more", "", 501).unwrap(), TodoChange::Invalid("the Todo list holds at most 500 items".to_string()));
    assert_eq!(store.todos().unwrap().len(), 500);
}
