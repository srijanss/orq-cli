use orq_tui::{dependency_statuses, render_feature_detail, DependencyStatus, FeatureDetail, FeatureRow};
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

fn draw(width: u16, height: u16, detail: &FeatureDetail) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| render_feature_detail(frame, Rect::new(0, 0, width, height), detail))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn detail() -> FeatureDetail {
    FeatureDetail {
        name: "beta".into(),
        description: Some("Does the second thing.".into()),
        status: "pending".into(),
        test_file: Some("-p orq-cli --test beta".into()),
        dependencies: vec![
            DependencyStatus { name: "alpha".into(), status: Some("completed".into()) },
            DependencyStatus { name: "ghost".into(), status: None },
        ],
    }
}

#[test]
fn the_detail_shows_description_status_test_file_and_each_dependency_with_its_status() {
    let text = draw(60, 10, &detail()).join("\n");

    assert!(text.contains("beta"), "{text}");
    assert!(text.contains("Does the second thing."), "{text}");
    assert!(text.contains("pending"), "{text}");
    assert!(text.contains("-p orq-cli --test beta"), "{text}");
    assert!(text.contains("alpha") && text.contains("completed"), "{text}");
}

#[test]
fn an_absent_description_shows_a_placeholder_and_an_absent_test_file_is_omitted() {
    let bare = FeatureDetail {
        description: None,
        test_file: None,
        dependencies: vec![],
        ..detail()
    };

    let text = draw(60, 10, &bare).join("\n");

    assert!(text.contains("no description"), "{text}");
    assert!(!text.contains("test file"), "{text}");
    assert!(!text.contains("depends on"), "{text}");
}

#[test]
fn a_dependency_missing_from_the_ledger_is_flagged_not_hidden() {
    let text = draw(60, 10, &detail()).join("\n");

    let ghost = text.lines().find(|l| l.contains("ghost")).expect(&text);
    assert!(ghost.contains("not in ledger"), "{ghost}");
}

#[test]
fn dependency_statuses_are_looked_up_in_the_rows_in_order() {
    let rows = vec![
        FeatureRow { name: "alpha".into(), status: "completed".into() },
        FeatureRow { name: "gamma".into(), status: "draft".into() },
    ];

    let deps = dependency_statuses(&["gamma".to_string(), "ghost".to_string(), "alpha".to_string()], &rows);

    assert_eq!(
        deps,
        vec![
            DependencyStatus { name: "gamma".into(), status: Some("draft".into()) },
            DependencyStatus { name: "ghost".into(), status: None },
            DependencyStatus { name: "alpha".into(), status: Some("completed".into()) },
        ]
    );
}

#[test]
fn control_characters_never_reach_the_screen_and_tiny_areas_do_not_panic() {
    let hostile = FeatureDetail {
        name: "a\u{1b}[31mb".into(),
        description: Some("line one\nline \u{7}two".into()),
        ..detail()
    };
    let text = draw(40, 10, &hostile).join("\n");
    assert!(!text.contains('\u{1b}') && !text.contains('\u{7}'), "{text:?}");

    for (w, h) in [(0, 0), (1, 1), (5, 2), (12, 3)] {
        let _ = draw(w, h, &detail());
    }
}
