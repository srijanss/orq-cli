use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use orq_features::{resolve_spec_file, spec_file_candidates, SpecFileError};

fn repo(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-spec-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn names(root: &Path, found: Vec<PathBuf>) -> Vec<String> {
    found.iter().map(|p| p.strip_prefix(root).unwrap().to_string_lossy().into_owned()).collect()
}

#[test]
fn spec_md_is_offered_first_but_any_markdown_file_is_accepted() {
    let root = repo("offer");
    fs::write(root.join("SPEC.md"), "# spec").unwrap();
    fs::write(root.join("a_notes.md"), "# notes").unwrap();
    fs::write(root.join("main.rs"), "").unwrap();

    let found = spec_file_candidates(&root);

    assert_eq!(names(&root.canonicalize().unwrap(), found), ["SPEC.md", "a_notes.md"]);
    assert!(resolve_spec_file(&root, "a_notes.md").is_ok());
}

#[test]
fn no_spec_md_is_not_required() {
    let root = repo("none");
    fs::write(root.join("design.md"), "# d").unwrap();

    let found = spec_file_candidates(&root);

    assert_eq!(names(&root.canonicalize().unwrap(), found), ["design.md"]);
}

#[test]
fn a_nested_relative_path_resolves_to_a_canonical_path_inside_the_repo() {
    let root = repo("nested");
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("docs/plan.md"), "# p").unwrap();

    let resolved = resolve_spec_file(&root, "docs/plan.md").unwrap();

    assert_eq!(resolved, root.canonicalize().unwrap().join("docs/plan.md"));
}

#[test]
fn paths_outside_the_repository_are_refused() {
    let root = repo("outside");
    let other = repo("outside-other");
    fs::write(other.join("evil.md"), "x").unwrap();

    let relative = resolve_spec_file(&root, "../orq-spec-outside-other-evil.md");
    let absolute = resolve_spec_file(&root, other.join("evil.md").to_str().unwrap());
    let dotdot = resolve_spec_file(&root, &format!("../{}/evil.md", other.file_name().unwrap().to_string_lossy()));

    assert!(matches!(relative, Err(SpecFileError::NotFound) | Err(SpecFileError::OutsideRepository)), "{relative:?}");
    assert_eq!(absolute, Err(SpecFileError::OutsideRepository));
    assert_eq!(dotdot, Err(SpecFileError::OutsideRepository));
}

#[test]
fn a_symlink_escaping_the_repository_is_refused_and_not_offered() {
    let root = repo("symlink");
    let other = repo("symlink-other");
    fs::write(other.join("secret.md"), "secret").unwrap();
    symlink(other.join("secret.md"), root.join("link.md")).unwrap();

    assert_eq!(resolve_spec_file(&root, "link.md"), Err(SpecFileError::OutsideRepository));
    assert!(spec_file_candidates(&root).is_empty());
}

#[test]
fn a_symlink_that_stays_inside_the_repository_is_accepted() {
    let root = repo("inside-link");
    fs::write(root.join("real.md"), "# r").unwrap();
    symlink(root.join("real.md"), root.join("alias.md")).unwrap();

    assert_eq!(resolve_spec_file(&root, "alias.md").unwrap(), root.canonicalize().unwrap().join("real.md"));
}

#[test]
fn only_markdown_files_are_accepted() {
    let root = repo("kinds");
    fs::write(root.join("main.rs"), "").unwrap();
    fs::create_dir_all(root.join("dir.md")).unwrap();

    assert_eq!(resolve_spec_file(&root, "main.rs"), Err(SpecFileError::NotMarkdown));
    assert_eq!(resolve_spec_file(&root, "dir.md"), Err(SpecFileError::NotAFile));
    assert_eq!(resolve_spec_file(&root, "missing.md"), Err(SpecFileError::NotFound));
    assert_eq!(resolve_spec_file(&root, "  "), Err(SpecFileError::NotFound));
}
