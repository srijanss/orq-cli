//! Projects are the git repositories directly inside the scan root (`~/code` by default).

mod web_deps;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use orq_remote::projects::{default_root, discover};

fn repo(root: &Path, name: &str) {
    fs::create_dir_all(root.join(name).join(".git")).unwrap();
}

#[test]
fn only_direct_git_repositories_are_found_with_a_stable_id_and_a_name_in_name_order() {
    let root = web_deps::private_temp_dir("discover");
    repo(&root, "zeta");
    repo(&root, "Alpha App");
    fs::create_dir(root.join("notes")).unwrap();
    fs::write(root.join("file.txt"), "x").unwrap();
    repo(&root.join("notes"), "nested");
    let found = discover(&root);
    let names: Vec<_> = found.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Alpha App", "zeta"]);
    assert_eq!(found[0].id, "alpha-app_416c70686120417070");
    assert_eq!(found[1].id, "zeta");
    assert_eq!(found[1].path, root.join("zeta"));
    // The same folders give the same ids on a second scan, which also picks up new repositories.
    repo(&root, "beta");
    let again = discover(&root);
    assert_eq!(again.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["alpha-app_416c70686120417070", "beta", "zeta"]);
}

#[test]
fn a_symlink_out_of_the_root_is_never_followed_and_unreadable_or_missing_roots_give_nothing() {
    let root = web_deps::private_temp_dir("links");
    let outside = web_deps::private_temp_dir("outside");
    repo(&outside, "secret");
    symlink(outside.join("secret"), root.join("linked")).unwrap();
    repo(&root, "real");
    let found = discover(&root);
    assert_eq!(found.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["real"]);
    assert!(discover(&root.join("does-not-exist")).is_empty());
}

#[test]
fn names_that_clash_after_cleaning_still_get_distinct_ids() {
    let root = web_deps::private_temp_dir("clash");
    repo(&root, "My App");
    repo(&root, "my-app");
    let ids: Vec<_> = discover(&root).into_iter().map(|p| p.id).collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
}

#[test]
fn the_default_root_is_the_code_folder_in_the_home_directory() {
    let root = default_root(Some(Path::new("/home/someone")), None);
    assert_eq!(root, Path::new("/home/someone/code"));
    assert_eq!(default_root(Some(Path::new("/home/someone")), Some(Path::new("/tmp/x"))), Path::new("/tmp/x"));
}

#[test]
fn an_id_depends_only_on_its_own_folder_name_and_never_equals_another_projects_id() {
    let root = web_deps::private_temp_dir("stable-ids");
    repo(&root, "My App");
    let before = discover(&root)[0].id.clone();
    repo(&root, "my-app");
    let ids: Vec<_> = discover(&root).into_iter().map(|p| (p.name, p.id)).collect();
    assert!(ids.contains(&("My App".to_string(), before)), "adding a lookalike changed an existing id: {ids:?}");
    // A folder named exactly like another project's generated id gets its own, different id.
    let generated = ids.iter().find(|(name, _)| name == "My App").unwrap().1.clone();
    repo(&root, &generated);
    let all: Vec<_> = discover(&root).into_iter().map(|p| p.id).collect();
    let unique: std::collections::HashSet<_> = all.iter().collect();
    assert_eq!(unique.len(), all.len(), "{all:?}");
}
