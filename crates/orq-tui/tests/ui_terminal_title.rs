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
