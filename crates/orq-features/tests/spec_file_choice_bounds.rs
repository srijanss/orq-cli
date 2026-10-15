use std::fs;
use std::path::PathBuf;

use orq_features::spec_file_choices;

fn repo(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-choicebounds-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn spec_md_stays_first_and_the_cap_is_deterministic_with_many_markdown_files() {
    let root = repo("many");
    fs::write(root.join("SPEC.md"), "x").unwrap();
    fs::create_dir_all(root.join("aaa")).unwrap();
    for n in 0..600 {
        fs::write(root.join("aaa").join(format!("n{n:04}.md")), "x").unwrap();
    }

    let first = spec_file_choices(&root);
    let second = spec_file_choices(&root);

    assert_eq!(first.len(), 500);
    assert_eq!(first[0], "SPEC.md");
    assert_eq!(first, second);
    assert_eq!(first[1], "aaa/n0000.md");
}

#[test]
fn a_huge_tree_of_other_files_is_cut_off_by_a_visit_budget() {
    let root = repo("budget");
    fs::write(root.join("SPEC.md"), "x").unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    for n in 0..12_000 {
        fs::write(root.join("src").join(format!("f{n:05}.rs")), "").unwrap();
    }
    fs::create_dir_all(root.join("zdocs")).unwrap();
    fs::write(root.join("zdocs/late.md"), "x").unwrap();

    let choices = spec_file_choices(&root);

    assert_eq!(choices, ["SPEC.md"]);
}
