//! `GET /api/projects/{id}/runs/{run}/design/image?path=...` asks the provider for one design image of the
//! run, with the path decoded, after the same peer and token checks as the run page.

use std::sync::Mutex;

use orq_core::remote_inbox::{handle_with, ProjectsProvider, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

/// Answers one image and records what it was asked.
struct Fake {
    asked: Mutex<Vec<(String, String, String)>>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn run_design_image(&self, id: &str, run: &str, path: &str) -> Result<Option<Value>, String> {
        self.asked
            .lock()
            .unwrap()
            .push((id.into(), run.into(), path.into()));
        match path {
            "round-1/desktop/index.png" => {
                Ok(Some(json!({"content_type": "image/png", "data": "AAAA"})))
            }
            "broken.png" => Err("/secret/path is unreadable".into()),
            _ => Ok(None),
        }
    }
}

fn roots() -> StorageRoots {
    // Each ask has its own folder: the tests run side by side in one process.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let base = orq_test_support::process_temp_dir()
        .join(format!("orq-design-image-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots {
        data: base.join("data"),
        config: base.join("config"),
        state: base.join("state"),
        cache: base.join("cache"),
    };
    roots.create_all().unwrap();
    roots
}

fn ask(fake: &Fake, path: &str, authorization: Option<&str>) -> (u16, Value) {
    let roots = roots();
    let config = RemoteConfig {
        token: "tok".into(),
    };
    let request = Request {
        method: "GET",
        path,
        peer: "127.0.0.1".parse().unwrap(),
        authorization,
    };
    let reply = handle_with(&roots, &config, "p", "p", &request, fake);
    (reply.status, reply.body)
}

#[test]
fn the_design_image_route_asks_the_provider_with_the_decoded_path_and_answers_what_it_finds() {
    let fake = Fake {
        asked: Mutex::new(Vec::new()),
    };

    assert_eq!(
        ask(
            &fake,
            "/api/projects/p-1/runs/r-1/design/image?path=round-1%2Fdesktop%2Findex.png",
            Some("Bearer tok")
        ),
        (200, json!({"content_type": "image/png", "data": "AAAA"}))
    );
    assert_eq!(
        ask(
            &fake,
            "/api/projects/p-1/runs/r-1/design/image?path=other.png",
            Some("Bearer tok")
        )
        .0,
        404
    );
    let (status, body) = ask(
        &fake,
        "/api/projects/p-1/runs/r-1/design/image?path=broken.png",
        Some("Bearer tok"),
    );
    assert_eq!(status, 500);
    assert!(
        !body.to_string().contains("/secret/path"),
        "the failure names no path: {body}"
    );
    assert_eq!(
        fake.asked.lock().unwrap().clone(),
        [
            (
                "p-1".into(),
                "r-1".into(),
                "round-1/desktop/index.png".into()
            ),
            ("p-1".into(), "r-1".into(), "other.png".into()),
            ("p-1".into(), "r-1".into(), "broken.png".into())
        ]
    );
}

#[test]
fn the_design_image_route_asks_nothing_without_a_path_or_a_token() {
    let fake = Fake {
        asked: Mutex::new(Vec::new()),
    };

    assert_eq!(
        ask(
            &fake,
            "/api/projects/p-1/runs/r-1/design/image",
            Some("Bearer tok")
        )
        .0,
        404
    );
    assert_eq!(
        ask(
            &fake,
            "/api/projects/p-1/runs/r-1/design/image?path=",
            Some("Bearer tok")
        )
        .0,
        404
    );
    assert_eq!(
        ask(
            &fake,
            "/api/projects/p-1/runs/r-1/design/image?path=round-1/desktop/index.png",
            None
        )
        .0,
        401
    );
    assert_eq!(
        ask(
            &fake,
            "/api/projects/p-1/runs/r-1/design/image?path=round-1/desktop/index.png",
            Some("Bearer nope")
        )
        .0,
        401
    );
    assert!(
        fake.asked.lock().unwrap().is_empty(),
        "{:?}",
        fake.asked.lock().unwrap()
    );
}
