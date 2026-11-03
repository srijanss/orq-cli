use orq_core::provider_observation::{classify_failure, parse_list};
use orq_core::review_lifecycle::{ObservationError, ReviewLifecycle};

const HEAD: &str = "1111111111111111111111111111111111111111";
const HOSTILE: &str = "\u{1b}]0;pwned\u{7}\u{1b}[2J\nstate: merged\ngate: resolved\r\u{9b}31m";

fn clean(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

#[test]
fn a_stored_error_is_one_line_without_control_characters() {
    let open = ReviewLifecycle::associate("github", "7", "https://h/o/r/pull/7", HEAD);
    for error in [ObservationError::Unavailable(HOSTILE.to_string()), ObservationError::PermissionDenied(HOSTILE.to_string())] {
        let next = open.apply(Err(error));
        let stored = next.last_error.unwrap();
        assert!(clean(&stored), "{stored:?}");
    }
}

#[test]
fn a_failure_detail_from_stderr_has_no_control_characters() {
    for error in [classify_failure(HOSTILE), classify_failure(&format!("HTTP 403 {HOSTILE}"))] {
        let (ObservationError::Unavailable(detail) | ObservationError::PermissionDenied(detail)) = error;
        assert!(clean(&detail), "{detail:?}");
    }
}

#[test]
fn a_review_address_with_control_characters_is_not_trusted() {
    let json = format!(r#"[{{"number":7,"url":"https://h/o/r/pull/7\u001b]0;pwned\u0007","state":"OPEN","headRefOid":"{HEAD}","isCrossRepository":false}}]"#);
    assert!(matches!(parse_list("github", &json), Err(ObservationError::Unavailable(_))));
    let spaced = format!(r#"[{{"number":7,"url":"https://h/o/r/pull/7 \n state","state":"OPEN","headRefOid":"{HEAD}","isCrossRepository":false}}]"#);
    assert!(matches!(parse_list("github", &spaced), Err(ObservationError::Unavailable(_))));
}
