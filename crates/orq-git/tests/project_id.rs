use orq_git::{project_id, resolve_project};
use orq_test_support::GitFixture;

#[test]
fn the_id_is_the_cleaned_name_and_a_stable_hash_of_the_common_dir_shared_by_worktrees() {
    let fixture = GitFixture::new();
    let worktree = fixture.add_linked_worktree("feature");
    let main = resolve_project(fixture.root()).unwrap();
    let linked = resolve_project(&worktree).unwrap();

    let id = project_id(&main);
    let (name, hash) = id.rsplit_once('-').expect("name-hash");
    assert_eq!(hash.len(), 8);
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')));
    assert_eq!(project_id(&linked), id, "every worktree of one repository has one id");
    assert_eq!(project_id(&main), id, "the id is stable");
}

#[test]
fn two_repositories_with_the_same_name_get_different_ids_and_odd_characters_are_cleaned() {
    let a = GitFixture::new();
    let b = GitFixture::new();
    let mut one = resolve_project(a.root()).unwrap();
    let mut two = resolve_project(b.root()).unwrap();
    one.name = "my app".to_string();
    two.name = "my app".to_string();
    assert_ne!(project_id(&one), project_id(&two));
    assert!(project_id(&one).starts_with("my_app-"));
}
