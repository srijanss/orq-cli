//! The build writes a stamp next to the bundle: a fingerprint of its inputs and of the bundle.

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

fn crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

#[test]
fn the_build_writes_a_stamp_holding_the_hash_of_the_bundle_it_made() {
    if Command::new("node").arg("--version").output().is_err() || !crate_dir().join("web-src/node_modules").exists() {
        eprintln!("skipped: node or the web-src dependencies are not installed");
        return;
    }
    let out = std::env::temp_dir().join(format!("orq-react-stamp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let built = Command::new("node").arg(crate_dir().join("web-src/build.mjs")).arg("--out").arg(out.join("app.react.js")).output().unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let stamp = std::fs::read_to_string(out.join("app.react.js.stamp")).expect("a stamp is written next to the bundle");
    let lines: Vec<&str> = stamp.lines().collect();
    assert_eq!(lines.len(), 2, "{stamp:?}");
    let sources = lines[0].strip_prefix("sources ").expect("first line");
    assert!(sources.len() == 64 && sources.chars().all(|c| c.is_ascii_hexdigit()), "{stamp:?}");
    let bundle: String = Sha256::digest(std::fs::read(out.join("app.react.js")).unwrap()).iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(lines[1], format!("bundle {bundle}"));
    assert!(stamp.ends_with('\n'));
}
