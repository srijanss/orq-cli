//! Tests on the page source: what the script is able to do with the token.

fn script() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.js")).expect("the page script exists")
}

/// The script without comments, so a remark about a forbidden thing is not
/// mistaken for using it.
fn code() -> String {
    script().lines().filter(|line| !line.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

#[test]
fn the_token_lives_in_memory_only() {
    let code = code();
    for banned in ["localStorage", "sessionStorage", "indexedDB", "document.cookie", "caches.", "cookieStore", "serviceWorker", "openDatabase"] {
        assert!(!code.contains(banned), "the script uses {banned}");
    }
    assert!(code.contains("let token = null"), "the token is held in one in-memory variable");
}

#[test]
fn the_token_never_reaches_a_url_a_log_or_the_history() {
    let code = code();
    for banned in ["location.search", "location.hash", "location.href", "URLSearchParams", "history.", "console.", "alert(", "window.name", "postMessage", "sendBeacon", "WebSocket", "EventSource", "XMLHttpRequest", "document.write", ".innerHTML", "eval(", "new Function"] {
        assert!(!code.contains(banned), "the script uses {banned}");
    }
    // Every fetch goes to a fixed same-origin path with no query string.
    for call in code.split("fetch(").skip(1) {
        let target: String = call.chars().take_while(|c| *c != ',' && *c != ')').collect();
        assert!(target.trim().starts_with("\"/api/") || target.trim().starts_with("API"), "fetch target {target:?} is not a fixed path");
        assert!(!target.contains('?') && !target.contains('+') && !target.contains('`'), "fetch target {target:?} is built from something");
    }
}

#[test]
fn every_request_is_a_get_with_the_token_in_an_authorization_header() {
    let code = code();
    assert!(code.contains("fetch("), "the script makes requests with fetch");
    assert!(code.contains("Authorization") && code.contains("\"Bearer \" + token"), "the token travels as a bearer header");
    assert!(code.contains("method: \"GET\""), "{code}");
    for banned in ["POST", "PUT", "DELETE", "PATCH", "credentials: \"include\"", "mode: \"no-cors\""] {
        assert!(!code.contains(banned), "the script uses {banned}");
    }
    assert_eq!(code.matches("method:").count(), code.matches("fetch(").count(), "each fetch states its method");
}

#[test]
fn a_401_clears_the_token_and_asks_again_with_one_message_that_blames_neither_side() {
    let code = code();
    assert!(code.contains("401"), "the script handles a 401");
    let after_401 = &code[code.find("401").unwrap()..];
    assert!(after_401.contains("token = null"), "a 401 clears the token");
    // One shared message for a refused token and a failed network.
    assert_eq!(code.matches("const SIGN_IN_FAILED").count(), 1, "one message constant");
    let message = code.split("const SIGN_IN_FAILED = \"").nth(1).and_then(|rest| rest.split('"').next()).expect("the message is a string constant");
    let lower = message.to_lowercase();
    for blamed in ["wrong", "invalid", "incorrect", "expired", "network", "offline", "unreachable", "timeout"] {
        assert!(!lower.contains(blamed), "the message says {blamed}: {message}");
    }
    assert!(code.matches("SIGN_IN_FAILED").count() >= 3, "used for a 401 and for a failed request alike");
}

#[test]
fn there_is_no_remember_me_and_the_field_is_a_password_input_that_is_not_stored_by_the_browser() {
    let code = code();
    assert!(!code.to_lowercase().contains("remember"));
    assert!(code.contains("type = \"password\"") || code.contains("type=\"password\""), "the token field is masked");
    assert!(code.contains("autocomplete") && code.contains("off"), "the browser is told not to store it");
    assert!(code.contains("value = \"\""), "the field is emptied once the token is taken");
}
