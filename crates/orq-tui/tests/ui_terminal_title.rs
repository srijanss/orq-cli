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

/// A writer that fails after `ok_writes` successful `write` calls, or on flush.
struct Faulty {
    ok_writes: usize,
    fail_flush: bool,
    written: Vec<u8>,
}

impl std::io::Write for Faulty {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.ok_writes == 0 {
            return Err(std::io::Error::other("write failed"));
        }
        self.ok_writes -= 1;
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.fail_flush {
            Err(std::io::Error::other("flush failed"))
        } else {
            Ok(())
        }
    }
}

fn pops_after_failed_enter(mut faulty: Faulty) -> usize {
    let mut state = orq_tui::TitleState::default();
    assert!(state.enter(&mut faulty).is_err());
    let mut out = Vec::new();
    state.leave(&mut out).unwrap();
    let first = out.len();
    state.leave(&mut out).unwrap();
    assert_eq!(out.len(), first, "a second leave must emit nothing");
    usize::from(first > 0)
}

#[test]
fn a_failure_before_the_push_is_written_pops_nothing() {
    let faulty = Faulty { ok_writes: 0, fail_flush: false, written: Vec::new() };

    assert_eq!(pops_after_failed_enter(faulty), 0);
}

#[test]
fn a_failure_setting_the_title_after_the_push_pops_once() {
    let faulty = Faulty { ok_writes: 1, fail_flush: false, written: Vec::new() };

    assert_eq!(pops_after_failed_enter(faulty), 1);
}

#[test]
fn a_flush_failure_after_the_push_pops_once() {
    let faulty = Faulty { ok_writes: usize::MAX, fail_flush: true, written: Vec::new() };

    assert_eq!(pops_after_failed_enter(faulty), 1);
}
