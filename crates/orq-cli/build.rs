//! Embeds the build's git commit and time, so `orq --version` and the web's Settings can tell one
//! build of the same version from another.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git").arg("-C").arg(&manifest).args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|text| !text.is_empty())
    };
    let commit = git(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=ORQ_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=ORQ_BUILD_TIME={}", rfc3339(build_seconds()));
    // Stamped again when the commit moves or the source changes; not on every build.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        watch(&Path::new(&dir).join("HEAD"));
    }
    if let Some(common) = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]) {
        let common = PathBuf::from(common);
        watch(&common.join("packed-refs"));
        if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
            watch(&common.join(branch));
        }
    }
}

fn watch(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
}

/// Now, or `SOURCE_DATE_EPOCH` for a reproducible build.
fn build_seconds() -> u64 {
    std::env::var("SOURCE_DATE_EPOCH").ok().and_then(|text| text.parse().ok()).unwrap_or_else(|| {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
    })
}

/// `seconds` since the epoch as `YYYY-MM-DDTHH:MM:SSZ`.
fn rfc3339(seconds: u64) -> String {
    let (days, rest) = ((seconds / 86_400) as i64, seconds % 86_400);
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest / 60 % 60, rest % 60)
}
