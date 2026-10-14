use orq_tui::{render_snapshot, PopupModel, PopupView};

#[test]
fn snapshot_does_not_pad_wide_characters_with_extra_spaces() {
    let view = PopupView::Project(PopupModel {
        project_name: "日本語".into(),
        repo_path: "/x/日本a".into(),
    });

    let screen = render_snapshot(&view, 20, 2);

    assert_eq!(screen, "日本語\n/x/日本a");
}
