//! Tests on the page source: what the script is able to do with the token.

fn script() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/main.jsx")).expect("the page source exists")
}

/// The source without comments, so a remark about a forbidden thing is not
/// mistaken for using it.
fn code() -> String {
    script().lines().filter(|line| !line.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

#[test]
fn the_token_lives_in_memory_and_this_tabs_session_storage_only() {
    let code = code();
    for banned in ["localStorage", "indexedDB", "document.cookie", "caches.", "cookieStore", "serviceWorker", "openDatabase"] {
        assert!(!code.contains(banned), "the source uses {banned}");
    }
    assert!(code.contains("useState(keptToken)"), "the token is held in one piece of state, restored from this tab only");
}

#[test]
fn the_token_never_reaches_a_url_a_log_or_the_history() {
    let code = code();
    for banned in ["location.search", "location.href", "URLSearchParams", "history.", "console.", "alert(", "window.name", "postMessage", "sendBeacon", "WebSocket", "EventSource", "XMLHttpRequest", "document.write", "innerHTML", "eval(", "new Function"] {
        assert!(!code.contains(banned), "the source uses {banned}");
    }
    // The page address may carry a run id, never the token.
    for line in code.lines().filter(|line| line.contains("location")) {
        assert!(!line.to_lowercase().contains("token"), "the address is built from the token: {line}");
    }
    // There are two fetches, a read and the confirmed action, and each takes the path it is given.
    assert_eq!(code.matches("fetch(").count(), 2);
    assert!(code.contains("fetch(path, { method: \"GET\"") && code.contains("fetch(path, { method: \"POST\"") && code.contains("get(path, token"), "each fetch takes a path");
    // Every path is a fixed same-origin one, plus an encoded run id, with no query string.
    for call in code.split("<Loaded ").skip(1) {
        let target: String = call.split("path={").nth(1).unwrap().chars().take_while(|c| *c != '}').collect();
        assert!(["API_REVIEWS", "API_PROJECTS", "API_SETTINGS", "API_AUDIT", "path", "base", "featuresPath", "runsPath", "timelinePath", "apiRun", "pagePath"].contains(&target.trim()), "path {target:?} is not fixed");
    }
    assert!(code.contains("API_REVIEWS + \"/\" + encodeURIComponent(runId)"), "the run id is encoded");
    assert!(code.contains("\"/api/projects/\" + encodeURIComponent(projectId) + \"/reviews\""), "the project id is encoded");
    assert!(!code.contains("API_REVIEWS + \"?"), "no query string");
}

#[test]
fn every_request_is_a_get_with_the_token_in_an_authorization_header() {
    let code = code();
    assert!(code.contains("Authorization") && code.contains("\"Bearer \" + token"), "the token travels as a bearer header");
    assert!(code.contains("method: \"GET\""), "{code}");
    for banned in ["PUT", "DELETE", "PATCH", "credentials: \"include\"", "mode: \"no-cors\""] {
        assert!(!code.contains(banned), "the source uses {banned}");
    }
    // The only other method is the action POST, sent from the one helper that Start, Pause and Resume share.
    assert_eq!(code.matches("POST").count(), 1);
    assert!(code.find("POST") > code.find("function actionRequest("));
    assert_eq!(code.matches("method:").count(), code.matches("fetch(").count(), "each fetch states its method");
}

#[test]
fn a_401_clears_the_token_and_asks_again_with_one_message_that_blames_neither_side() {
    let code = code();
    assert!(code.contains("401"), "the source handles a 401");
    assert!(code.contains("setToken(null)"), "a refused sign-in clears the token");
    // One shared message for a refused token and a failed network.
    assert_eq!(code.matches("const SIGN_IN_FAILED").count(), 1, "one message constant");
    let message = code.split("const SIGN_IN_FAILED = \"").nth(1).and_then(|rest| rest.split('"').next()).expect("the message is a string constant");
    let lower = message.to_lowercase();
    for blamed in ["wrong", "invalid", "incorrect", "expired", "network", "offline", "unreachable", "timeout"] {
        assert!(!lower.contains(blamed), "the message says {blamed}: {message}");
    }
    assert_eq!(code.matches("onSignedOut()").count(), 6, "used for a 401 on a page, on the project list, on the sidebar dropdown's list, on the combined features read, on the live stream and on an action, never for a failed request");
}

#[test]
fn there_is_no_remember_me_and_the_field_is_a_password_input_that_is_not_stored_by_the_browser() {
    let code = code();
    assert!(!code.to_lowercase().contains("remember"));
    assert!(code.contains("type=\"password\""), "the token field is masked");
    assert!(code.contains("autoComplete=\"off\""), "the browser is told not to store it");
    assert!(code.contains("value = \"\""), "the field is emptied once the token is taken");
}
