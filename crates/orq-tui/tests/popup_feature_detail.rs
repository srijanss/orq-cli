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

fn draw_in(frame_w: u16, frame_h: u16, area: Rect, detail: &FeatureDetail) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(frame_w, frame_h)).unwrap();
    terminal.draw(|frame| render_feature_detail(frame, area, detail)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..frame_h)
        .map(|y| (0..frame_w).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

#[test]
fn a_long_name_or_dependency_name_never_hides_the_status_or_the_missing_flag() {
    let long = "n".repeat(60);
    let narrow = FeatureDetail {
        name: long.clone(),
        dependencies: vec![
            DependencyStatus { name: long.clone(), status: None },
            DependencyStatus { name: long, status: Some("completed".into()) },
        ],
        ..detail()
    };

    let lines = draw(30, 10, &narrow);

    assert!(lines[0].contains("pending") && lines[0].contains('…'), "{lines:?}");
    let missing = lines.iter().find(|l| l.contains("not in ledger")).expect("flag visible");
    assert!(missing.contains('…'), "{missing}");
    assert!(lines.iter().any(|l| l.contains("completed")), "{lines:?}");
    assert!(lines.iter().all(|l| l.chars().count() <= 30), "{lines:?}");
}

#[test]
fn duplicate_row_names_resolve_to_the_first_match() {
    let rows = vec![
        FeatureRow { name: "a".into(), status: "draft".into() },
        FeatureRow { name: "a".into(), status: "completed".into() },
    ];

    let deps = dependency_statuses(&["a".to_string()], &rows);

    assert_eq!(deps[0].status.as_deref(), Some("draft"));
}

#[test]
fn an_offset_area_is_drawn_in_place_and_a_disjoint_or_partial_one_is_clipped() {
    let offset = draw_in(40, 8, Rect::new(10, 3, 20, 5), &detail());
    assert!(offset[0].is_empty() && offset[2].is_empty(), "{offset:?}");
    assert!(offset[3].starts_with("          beta"), "{offset:?}");
    assert!(offset[3..].iter().all(|l| l.chars().count() <= 30), "{offset:?}");

    let disjoint = draw_in(40, 8, Rect::new(100, 100, 20, 5), &detail());
    assert!(disjoint.iter().all(String::is_empty), "{disjoint:?}");

    let partial = draw_in(40, 8, Rect::new(30, 6, 20, 5), &detail());
    assert!(partial[6].starts_with(&" ".repeat(30)) && partial[6].contains("beta"), "{partial:?}");
    assert!(partial.iter().all(|l| l.chars().count() <= 40), "{partial:?}");
}

#[test]
fn wide_and_combining_text_is_cut_on_whole_clusters_and_keeps_the_status() {
    let unicode = FeatureDetail {
        name: "日本語日本語日本語e\u{301}e\u{301}".into(),
        ..detail()
    };

    let lines = draw(14, 6, &unicode);

    assert!(lines[0].contains("pending"), "{lines:?}");
    assert!(lines[0].contains('…'), "{lines:?}");
    assert!(!lines[0].starts_with('\u{301}'), "{lines:?}");
}
