use orq_tui::{enter_title_sequence, leave_title_sequence, TERMINAL_TITLE};

#[test]
fn entering_saves_the_current_title_then_sets_orq() {
    let mut out = Vec::new();
    enter_title_sequence(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();

    assert_eq!(TERMINAL_TITLE, "ORQ");
    let push = text.find("\x1b[22;2t").expect("title is pushed before it is replaced");
    let set = text.find("ORQ").expect("title is set");
    assert!(push < set, "{text:?}");
}

#[test]
fn leaving_restores_the_saved_title() {
    let mut out = Vec::new();
    leave_title_sequence(&mut out).unwrap();

    assert_eq!(String::from_utf8(out).unwrap(), "\x1b[23;2t");
}

#[test]
fn leaving_without_a_successful_entry_pops_nothing() {
    let mut state = orq_tui::TitleState::default();
    let mut out = Vec::new();

    state.leave(&mut out).unwrap();

    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn a_session_pops_exactly_once_after_it_pushed() {
    let mut state = orq_tui::TitleState::default();
    let mut out = Vec::new();
    state.enter(&mut out).unwrap();
    out.clear();

    state.leave(&mut out).unwrap();
    assert_eq!(String::from_utf8(out.clone()).unwrap(), "\x1b[23;2t");
    out.clear();
    state.leave(&mut out).unwrap();

    assert!(out.is_empty(), "a second leave must not pop again");
}
