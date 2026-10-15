use std::fs;
use std::path::PathBuf;

use orq_features::spec_file_choices;

fn repo(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-choices-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn markdown_files_are_listed_relative_with_spec_first_and_noise_skipped() {
    let root = repo("list");
    fs::write(root.join("SPEC.md"), "x").unwrap();
    fs::write(root.join("b.md"), "x").unwrap();
    fs::write(root.join("notes.txt"), "x").unwrap();
    fs::create_dir_all(root.join("docs/deep")).unwrap();
    fs::write(root.join("docs/mvp2.md"), "x").unwrap();
    fs::write(root.join("docs/deep/more.markdown"), "x").unwrap();
    fs::create_dir_all(root.join("target")).unwrap();
    fs::write(root.join("target/skip.md"), "x").unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git/skip.md"), "x").unwrap();
    fs::create_dir_all(root.join("node_modules")).unwrap();
    fs::write(root.join("node_modules/skip.md"), "x").unwrap();

    let choices = spec_file_choices(&root);

    assert_eq!(choices, ["SPEC.md", "b.md", "docs/deep/more.markdown", "docs/mvp2.md"]);
}

#[test]
fn a_symlink_leaving_the_repository_is_not_offered() {
    let root = repo("link");
    let outside = repo("link-outside");
    fs::write(outside.join("secret.md"), "x").unwrap();
    std::os::unix::fs::symlink(outside.join("secret.md"), root.join("leak.md")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("linked-dir")).unwrap();
    fs::write(root.join("ok.md"), "x").unwrap();

    assert_eq!(spec_file_choices(&root), ["ok.md"]);
}
