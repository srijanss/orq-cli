//! The React source builds, offline, to the bundle that is committed and served.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

fn crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
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
    if !web_deps::ready() {
        return;
    }
    let out = std::env::temp_dir().join(format!("orq-react-build-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let built = Command::new("node").arg(crate_dir().join("web-src/build.mjs")).arg("--out").arg(out.join("app.react.js")).output().unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    assert_eq!(std::fs::read(out.join("app.react.js")).unwrap(), committed, "web/app.react.js is out of date: run `node crates/orq-remote/web-src/build.mjs`");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// SHA-256 over every build input (the script, both package files, the sources), each as
/// its path relative to web-src, a zero byte, its bytes, a zero byte, in path order.
fn source_fingerprint() -> String {
    use sha2::{Digest, Sha256};
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let root = crate_dir().join("web-src");
    let mut files: Vec<PathBuf> = ["build.mjs", "package.json", "package-lock.json"].iter().map(|name| root.join(name)).collect();
    walk(&root.join("src"), &mut files);
    let mut names: Vec<(String, PathBuf)> = files.into_iter().map(|path| (path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"), path)).collect();
    names.sort();
    let mut hasher = Sha256::new();
    for (name, path) in names {
        hasher.update(name.as_bytes());
        hasher.update([0u8]);
        hasher.update(std::fs::read(path).unwrap());
        hasher.update([0u8]);
    }
    hex(&hasher.finalize())
}

#[test]
fn the_stamp_ties_the_committed_bundle_to_the_sources_without_node() {
    use sha2::{Digest, Sha256};
    let stamp = std::fs::read_to_string(crate_dir().join("web/app.react.js.stamp")).expect("the build writes web/app.react.js.stamp");
    let bundle = hex(&Sha256::digest(std::fs::read(crate_dir().join("web/app.react.js")).unwrap()));
    let expected = format!("sources {}\nbundle {bundle}\n", source_fingerprint());
    assert_eq!(stamp, expected, "the sources or the bundle changed without a rebuild: run `node crates/orq-remote/web-src/build.mjs`");
}

#[test]
fn the_source_never_stores_anything_or_writes_html_from_text() {
    for (path, text) in sources() {
        for banned in ["localStorage", "indexedDB", "document.cookie", "caches.", "serviceWorker", "innerHTML", "dangerouslySetInnerHTML", "eval("] {
            assert!(!text.contains(banned), "{} uses {banned}", path.display());
        }
        // The tab's own session storage holds the token across a refresh, and only main.jsx may touch it.
        let is_main = path.file_name().map_or(false, |n| n == "main.jsx");
        assert!(is_main || !text.contains("sessionStorage"), "{} uses sessionStorage", path.display());
    }
}
