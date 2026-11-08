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

#[test]
fn a_symlinked_root_is_allowed_on_purpose_and_projects_are_reported_under_its_real_location() {
    let real = web_deps::private_temp_dir("real-root");
    repo(&real, "app");
    let holder = web_deps::private_temp_dir("link-holder");
    symlink(&real, holder.join("code")).unwrap();
    let found = discover(&holder.join("code"));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, fs::canonicalize(&real).unwrap().join("app"), "paths must name the real location, not the link");
}

#[test]
fn a_linked_worktree_marker_file_counts_but_a_symlinked_git_entry_does_not() {
    let root = web_deps::private_temp_dir("markers");
    fs::create_dir(root.join("worktree")).unwrap();
    fs::write(root.join("worktree/.git"), "gitdir: /elsewhere/.git/worktrees/w\n").unwrap();
    let outside = web_deps::private_temp_dir("markers-outside");
    fs::create_dir(outside.join(".git")).unwrap();
    fs::create_dir(root.join("borrowed")).unwrap();
    symlink(outside.join(".git"), root.join("borrowed/.git")).unwrap();
    let names: Vec<_> = discover(&root).into_iter().map(|p| p.name).collect();
    assert_eq!(names, ["worktree"]);
}

#[test]
fn a_root_that_is_a_file_gives_no_projects() {
    let root = web_deps::private_temp_dir("file-root");
    fs::write(root.join("plain"), "x").unwrap();
    assert!(discover(&root.join("plain")).is_empty());
}

#[test]
fn a_root_or_a_project_folder_that_cannot_be_read_is_skipped_without_failing() {
    use std::os::unix::fs::PermissionsExt;
    let root = web_deps::private_temp_dir("denied");
    repo(&root, "open");
    repo(&root, "closed");
    let closed = root.join("closed");
    fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).unwrap();
    let restricted = fs::read_dir(&closed).is_err();
    let names: Vec<_> = discover(&root).into_iter().map(|p| p.name).collect();
    fs::set_permissions(&closed, fs::Permissions::from_mode(0o700)).unwrap();
    if restricted {
        assert_eq!(names, ["open"]);
    } else {
        eprintln!("skipped the permission check: this user bypasses file permissions");
    }
    fs::set_permissions(&root, fs::Permissions::from_mode(0o000)).unwrap();
    let blocked = fs::read_dir(&root).is_err();
    let none = discover(&root);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    if blocked {
        assert!(none.is_empty());
    }
}

#[test]
fn a_plain_group_folder_lends_the_repositories_directly_inside_it_but_no_deeper() {
    let root = web_deps::private_temp_dir("group");
    repo(&root, "top");
    fs::create_dir(root.join("pitstop")).unwrap();
    repo(&root.join("pitstop"), "payment_hub");
    repo(&root.join("pitstop"), "guard-check");
    fs::create_dir(root.join("pitstop").join("deeper")).unwrap();
    repo(&root.join("pitstop").join("deeper"), "too-deep");
    let found = discover(&root);
    let names: Vec<_> = found.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["pitstop/guard-check", "pitstop/payment_hub", "top"]);
    assert_eq!(found[1].path, root.join("pitstop").join("payment_hub"));
    let ids: std::collections::HashSet<_> = found.iter().map(|p| p.id.clone()).collect();
    assert_eq!(ids.len(), 3);
    // A repository is a project; its own subfolders are never looked into.
    repo(&root.join("top"), "inner");
    assert_eq!(discover(&root).len(), 3);
}

#[test]
fn a_nested_project_with_long_names_still_gets_a_short_stable_distinct_id_the_page_accepts() {
    let root = web_deps::private_temp_dir("long");
    let group = "a-group-folder-with-a-long-name";
    fs::create_dir(root.join(group)).unwrap();
    let shared = "a-repository-name-that-is-long-and-shared";
    repo(&root.join(group), &format!("{shared}-one"));
    repo(&root.join(group), &format!("{shared}-two"));
    let found = discover(&root);
    assert_eq!(found.len(), 2);
    for project in &found {
        assert!(project.id.len() <= 100, "{}: {}", project.id.len(), project.id);
        assert!(project.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'), "{}", project.id);
    }
    assert_ne!(found[0].id, found[1].id);
    assert_eq!(found.iter().map(|p| p.id.clone()).collect::<Vec<_>>(), discover(&root).iter().map(|p| p.id.clone()).collect::<Vec<_>>());
}

#[test]
fn inside_a_group_a_linked_folder_a_linked_git_marker_and_an_unreadable_folder_are_skipped() {
    use std::os::unix::fs::PermissionsExt;
    let root = web_deps::private_temp_dir("group-edges");
    let outside = web_deps::private_temp_dir("group-edges-outside");
    repo(&outside, "secret");
    fs::create_dir(root.join("group")).unwrap();
    repo(&root.join("group"), "good");
    symlink(outside.join("secret"), root.join("group/linked")).unwrap();
    fs::create_dir(root.join("group/borrowed")).unwrap();
    symlink(outside.join("secret/.git"), root.join("group/borrowed/.git")).unwrap();
    fs::create_dir(root.join("sealed")).unwrap();
    repo(&root.join("sealed"), "hidden");
    let sealed = root.join("sealed");
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o000)).unwrap();
    let restricted = fs::read_dir(&sealed).is_err();
    let names: Vec<_> = discover(&root).into_iter().map(|p| p.name).collect();
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o700)).unwrap();
    if restricted {
        assert_eq!(names, ["group/good"]);
    } else {
        assert_eq!(names, ["group/good", "sealed/hidden"]);
    }
}

#[test]
fn a_direct_repository_with_a_very_long_plain_name_gets_a_bounded_stable_id() {
    let root = web_deps::private_temp_dir("long-plain");
    let name = "a".repeat(129);
    repo(&root, &name);
    repo(&root, &"a".repeat(130));
    let found = discover(&root);
    assert_eq!(found.len(), 2);
    for project in &found {
        assert!(project.id.len() <= 100, "{}", project.id.len());
    }
    assert_ne!(found[0].id, found[1].id);
    assert_eq!(found[0].id, discover(&root)[0].id);
}
