//! The React source builds, offline, to the bundle that is committed and served.

use std::path::{Path, PathBuf};
use std::process::Command;

fn crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn node_available() -> bool {
    Command::new("node").arg("--version").output().is_ok()
}

fn sources() -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if matches!(path.extension().and_then(|e| e.to_str()), Some("js" | "jsx")) {
                let text = std::fs::read_to_string(&path).unwrap();
                out.push((path, text));
            }
        }
    }
    let mut out = Vec::new();
    walk(&crate_dir().join("web-src/src"), &mut out);
    assert!(!out.is_empty(), "web-src/src has no source files");
    out
}

#[test]
fn the_committed_bundle_is_what_the_source_builds_to() {
    let committed = std::fs::read(crate_dir().join("web/app.react.js")).expect("the bundle is committed");
    assert!(!committed.is_empty());
    if !node_available() || !crate_dir().join("web-src/node_modules").exists() {
        eprintln!("skipped: node or the web-src dependencies are not installed");
        return;
    }
    let out = std::env::temp_dir().join(format!("orq-react-build-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let built = Command::new("node").arg(crate_dir().join("web-src/build.mjs")).arg("--out").arg(out.join("app.react.js")).output().unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    assert_eq!(std::fs::read(out.join("app.react.js")).unwrap(), committed, "web/app.react.js is out of date: run `node crates/orq-remote/web-src/build.mjs`");
}

#[test]
fn the_source_never_stores_anything_or_writes_html_from_text() {
    for (path, text) in sources() {
        for banned in ["localStorage", "sessionStorage", "indexedDB", "document.cookie", "caches.", "serviceWorker", "innerHTML", "dangerouslySetInnerHTML", "eval("] {
            assert!(!text.contains(banned), "{} uses {banned}", path.display());
        }
    }
}
