//! Application paths such as /projects/<id>/reviews are answered with the page, so a reload or a
//! shared address opens the right place; everything else keeps its old answer.

fn page() -> &'static str {
    orq_remote::page_asset("127.0.0.1".parse().unwrap(), "GET", "/").unwrap().body
}

fn served(peer: &str, method: &str, path: &str) -> Option<&'static str> {
    orq_remote::page_asset(peer.parse().unwrap(), method, path).map(|asset| asset.body)
}

#[test]
fn application_paths_are_answered_with_the_page() {
    for path in ["/dashboard", "/settings", "/projects", "/projects/app", "/projects/app/reviews", "/projects/my%20app/features", "/reviews/run-1", "/projects/app/reviews/run-1"] {
        for method in ["GET", "HEAD"] {
            assert_eq!(served("127.0.0.1", method, path), Some(page()), "{method} {path}");
        }
    }
}

#[test]
fn the_api_and_unknown_or_odd_paths_are_not_answered_with_the_page() {
    for path in ["/api/projects", "/api/reviews", "/api/projects/app/reviews", "/nope", "/dashboardx", "/projects//x", "/projects/../api/projects", "/projects/a/../../api", "/reviews/run-1/extra", "/reviews/", "/projects/app/unknown", "/projects/app/reviews/run-1/extra", "/projects/app/reviews/..", "/projects/app/features/x", "/settings/", "//dashboard"] {
        assert_eq!(served("127.0.0.1", "GET", path), None, "{path}");
    }
}

#[test]
fn application_paths_keep_the_same_gates_as_the_page() {
    assert_eq!(served("127.0.0.1", "POST", "/dashboard"), None);
    assert_eq!(served("127.0.0.1", "DELETE", "/projects/app"), None);
    assert_eq!(served("8.8.8.8", "GET", "/dashboard"), None);
    assert_eq!(served("192.168.1.5", "GET", "/projects/app/reviews"), None);
    assert_eq!(served("100.64.0.1", "GET", "/dashboard"), Some(page()));
}

#[test]
fn a_run_its_next_pages_and_its_logs_open_from_a_reload_or_a_shared_address() {
    for path in [
        "/projects/app/orchestrations/run-1",
        "/projects/app/orchestrations/run-1/after/e0",
        "/projects/app/orchestrations/run-1/logs",
        "/projects/app/orchestrations/run-1/logs/implement.log",
        "/projects/app/orchestrations/run-1/logs/my%20log.log/after/500",
    ] {
        for method in ["GET", "HEAD"] {
            assert_eq!(served("127.0.0.1", method, path), Some(page()), "{method} {path}");
        }
    }
    for path in [
        "/projects/app/orchestrations/run-1/extra",
        "/projects/app/orchestrations/run-1/after",
        "/projects/app/orchestrations/run-1/after/e0/extra",
        "/projects/app/orchestrations/run-1/logs/",
        "/projects/app/orchestrations/run-1/logs/f.log/after",
        "/projects/app/orchestrations/run-1/logs/f.log/before/1",
        "/projects/app/orchestrations/run-1/logs/f.log/after/1/extra",
        "/projects/app/orchestrations/../logs",
        "/projects/app/orchestrations/run-1/logs/..",
        "/projects/app/orchestrations//logs",
    ] {
        assert_eq!(served("127.0.0.1", "GET", path), None, "{path}");
    }
}
