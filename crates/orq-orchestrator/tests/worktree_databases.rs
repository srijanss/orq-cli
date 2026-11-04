use std::fs;
use std::path::PathBuf;

use orq_orchestrator::{derive_worktree_databases, derive_worktree_databases_with};

fn main_checkout(tag: &str, dotenv: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-worktree-db-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(".env"), dotenv).unwrap();
    dir
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| name.to_string()).collect()
}

fn pairs(found: &orq_orchestrator::WorktreeDatabases) -> Vec<(&str, &str)> {
    found.env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

#[test]
fn each_variable_with_a_value_gets_the_main_name_plus_the_branch_slug() {
    let main = main_checkout("plain", "DB_NAME=shop\nB2C_DB_NAME=\"shop_b2c\"\nDB_PWD=secret\n");

    let found = derive_worktree_databases(&main, &names(&["DB_NAME", "B2C_DB_NAME"]), "fix--COM-01", true);

    assert_eq!(pairs(&found), [("DB_NAME", "shop_fix_com_01"), ("B2C_DB_NAME", "shop_b2c_fix_com_01")]);
    assert!(found.notes.is_empty(), "{:?}", found.notes);
}

#[test]
fn the_last_assignment_wins_and_export_quotes_and_trailing_comments_are_understood() {
    let main = main_checkout("syntax", "# DB_NAME=commented\nDB_NAME=old\nexport DB_NAME = 'shop'  # the main one\n");

    let found = derive_worktree_databases(&main, &names(&["DB_NAME"]), "feat/x", true);

    assert_eq!(pairs(&found), [("DB_NAME", "shop_feat_x")]);
}

#[test]
fn a_variable_without_a_value_is_skipped_quietly_unless_the_repo_named_it() {
    let main = main_checkout("missing", "OTHER=1\n");

    let implicit = derive_worktree_databases(&main, &names(&["DB_NAME"]), "b", false);
    let explicit = derive_worktree_databases(&main, &names(&["DB_NAME"]), "b", true);

    assert!(implicit.env.is_empty() && implicit.notes.is_empty());
    assert!(explicit.env.is_empty());
    assert!(explicit.notes.iter().any(|n| n.contains("DB_NAME")), "{:?}", explicit.notes);
}

#[test]
fn a_main_checkout_without_a_dotenv_file_isolates_nothing() {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-worktree-db-{}-nofile", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let found = derive_worktree_databases(&dir, &names(&["DB_NAME"]), "b", false);

    assert!(found.env.is_empty() && found.notes.is_empty());
}

#[test]
fn a_long_branch_is_shortened_with_a_hash_so_the_test_database_name_fits_64_characters() {
    let main = main_checkout("long", "DB_NAME=shop\n");
    let branch = "fix--COM-01-a-very-long-feature-name-that-goes-on-and-on-and-on-forever";

    let found = derive_worktree_databases(&main, &names(&["DB_NAME"]), branch, true);

    let value = &found.env[0].1;
    assert!(format!("test_{value}").len() <= 64, "{value}");
    assert!(value.starts_with("shop_fix_com_01"), "{value}");
    let other = derive_worktree_databases(&main, &names(&["DB_NAME"]), &format!("{branch}-2"), true);
    assert_ne!(found.env, other.env, "two long branches must not share a database");
}

#[test]
fn a_main_name_with_no_room_for_a_suffix_is_skipped_with_a_note_that_leaves_out_the_value() {
    let long = "x".repeat(58);
    let main = main_checkout("roomless", &format!("DB_NAME={long}\n"));

    let found = derive_worktree_databases(&main, &names(&["DB_NAME"]), "b", false);

    assert!(found.env.is_empty());
    assert!(found.notes.iter().any(|n| n.contains("DB_NAME") && n.contains("too long") && !n.contains(&long)), "{:?}", found.notes);
}

#[test]
fn a_dotenv_file_that_exists_but_cannot_be_read_is_reported_instead_of_silently_isolating_nothing() {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-worktree-db-{}-unreadable", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join(".env")).unwrap();

    let found = derive_worktree_databases(&dir, &names(&["DB_NAME"]), "b", false);

    assert!(found.env.is_empty());
    assert!(found.notes.iter().any(|n| n.contains("dotenv") && n.contains("not isolated")), "{:?}", found.notes);
}

#[test]
fn a_value_that_is_not_a_plain_database_name_is_never_passed_on() {
    let main = main_checkout("unsafe", "DB_NAME=shop;drop\n");

    let found = derive_worktree_databases(&main, &names(&["DB_NAME"]), "b", true);

    assert!(found.env.is_empty());
    assert!(found.notes.iter().all(|n| !n.contains("drop")), "{:?}", found.notes);
}

fn drop_names(found: &orq_orchestrator::WorktreeDatabases) -> Vec<(&str, &str)> {
    found.names.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

#[test]
fn the_database_in_a_url_variable_is_renamed_and_everything_else_in_the_url_is_kept() {
    let main = main_checkout("url", "DATABASE_URL=postgres://app:s3cret@db.local:5432/shop?sslmode=disable\nMYSQL_URL='mysql://root@127.0.0.1/shop_b2c'\n");

    let found = derive_worktree_databases(&main, &names(&["DATABASE_URL", "MYSQL_URL"]), "fix--COM-01", true);

    assert_eq!(
        pairs(&found),
        [
            ("DATABASE_URL", "postgres://app:s3cret@db.local:5432/shop_fix_com_01?sslmode=disable"),
            ("MYSQL_URL", "mysql://root@127.0.0.1/shop_b2c_fix_com_01"),
        ]
    );
    assert_eq!(drop_names(&found), [("DATABASE_URL", "shop_fix_com_01"), ("MYSQL_URL", "shop_b2c_fix_com_01")]);
    assert!(found.notes.is_empty(), "{:?}", found.notes);
}

#[test]
fn a_plain_name_is_also_the_name_to_drop() {
    let main = main_checkout("names", "DB_NAME=shop\n");

    let found = derive_worktree_databases(&main, &names(&["DB_NAME"]), "fix--COM-01", true);

    assert_eq!(drop_names(&found), [("DB_NAME", "shop_fix_com_01")]);
}

#[test]
fn a_sqlite_file_url_keeps_its_directory_and_extension_and_a_url_without_a_database_is_left_alone() {
    let main = main_checkout("sqlite", "DB_FILE=sqlite:///var/data/shop.sqlite3\nMEM=sqlite:///:memory:\nCACHE=redis://:hunter2@cache:6379\n");

    let found = derive_worktree_databases(&main, &names(&["DB_FILE", "MEM", "CACHE"]), "fix--COM-01", true);

    assert_eq!(pairs(&found), [("DB_FILE", "sqlite:///var/data/shop_fix_com_01.sqlite3")]);
    assert_eq!(drop_names(&found), [("DB_FILE", "shop_fix_com_01")]);
    assert!(found.notes.iter().any(|n| n.contains("MEM")) && found.notes.iter().any(|n| n.contains("CACHE")), "{:?}", found.notes);
    assert!(found.notes.iter().all(|n| !n.contains("hunter2")), "no secret in a note: {:?}", found.notes);
}

#[test]
fn a_name_template_places_the_main_name_and_the_branch_part_where_the_repo_wants_them() {
    let main = main_checkout("template", "DB_NAME=shop\nDATABASE_URL=postgres://u@h/shop\n");

    let found = derive_worktree_databases_with(&main, &names(&["DB_NAME", "DATABASE_URL"]), "fix--COM-01", true, Some("wt_{branch}__{name}"));

    assert_eq!(pairs(&found), [("DB_NAME", "wt_fix_com_01__shop"), ("DATABASE_URL", "postgres://u@h/wt_fix_com_01__shop")]);
    assert_eq!(drop_names(&found), [("DB_NAME", "wt_fix_com_01__shop"), ("DATABASE_URL", "wt_fix_com_01__shop")]);
}

#[test]
fn a_template_without_the_branch_placeholder_or_with_odd_characters_isolates_nothing() {
    let main = main_checkout("bad-template", "DB_NAME=shop\n");

    for template in ["{name}_fixed", "{name}-{branch}", "{name}_{branch}; drop"] {
        let found = derive_worktree_databases_with(&main, &names(&["DB_NAME"]), "b1", true, Some(template));
        assert!(found.names.is_empty(), "{template}: {:?}", found.names);
        assert!(found.notes.iter().any(|n| n.contains("db_name_template")), "{template}: {:?}", found.notes);
    }
}

#[test]
fn a_template_that_repeats_the_branch_placeholder_isolates_nothing() {
    let main = main_checkout("template-twice", "DB_NAME=shop\n");

    let found = derive_worktree_databases_with(&main, &names(&["DB_NAME"]), &format!("feature/{}", "x".repeat(80)), true, Some("{name}_{branch}_{branch}"));

    assert!(found.names.is_empty() && found.env.is_empty(), "{:?}", found.names);
    assert!(found.notes.iter().any(|n| n.contains("db_name_template")), "{:?}", found.notes);
}

#[test]
fn a_url_whose_options_select_a_database_is_not_isolated() {
    let main = main_checkout("url-override", "PG=postgres://u@h/shop?dbname=production\nPG2=postgres://u@h/shop?sslmode=disable&DBName=production\nOK=postgres://u@h/shop?sslmode=disable\n");

    let found = derive_worktree_databases(&main, &names(&["PG", "PG2", "OK"]), "b1", true);

    assert_eq!(drop_names(&found), [("OK", "shop_b1")]);
    assert!(found.notes.iter().any(|n| n.contains("PG ")) && found.notes.iter().any(|n| n.contains("PG2")), "{:?}", found.notes);
    assert!(found.notes.iter().all(|n| !n.contains("production")), "{:?}", found.notes);
}

#[test]
fn a_url_option_name_that_is_percent_encoded_is_not_trusted() {
    let main = main_checkout("url-encoded", "PG=postgres://u@h/shop?%64bname=production\n");

    let found = derive_worktree_databases(&main, &names(&["PG"]), "b1", true);

    assert!(found.names.is_empty() && found.notes.iter().any(|n| n.contains("PG")), "{:?}", found.notes);
}

#[test]
fn a_long_branch_still_fits_64_characters_with_a_template() {
    let main = main_checkout("template-long", "DB_NAME=shop\n");

    let found = derive_worktree_databases_with(&main, &names(&["DB_NAME"]), &format!("feature/{}", "x".repeat(80)), true, Some("wt_{branch}__{name}"));

    let name = &found.names[0].1;
    assert!(name.len() + "test_".len() <= 64 && name.ends_with("__shop") && name.starts_with("wt_"), "{name}");
}
