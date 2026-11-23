//! `orq design review --repo <path> --feature <name>`: reviews the page as it is, with the
//! project's design reviewer, and exits 0 on pass, 1 on changes required, 2 when blocked or
//! invalid. These tests need no browser and no model: a page that never starts blocks the review
//! before any reviewer, and the reviewer is a fake pi on `PATH`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use orq_core::project_settings::{ProjectAgent, ProjectSettings, SaveOutcome};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_test_support::{GitFixture, TempDir};

const PI_MODELS: &str = "provider  model          context  max-out  thinking  images\nopenai    gpt-6.1-sol    400K     128K     yes       yes\n";

struct Fixture {
    repo: GitFixture,
    home: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let home = TempDir::new("orq-design-review-cli");
        let bin = home.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let calls = home.path().join("pi-calls");
        fs::write(
            bin.join("pi"),
            format!(
                "#!/bin/sh\nif [ \"$1\" = --list-models ]; then printf '%s' '{PI_MODELS}'; exit 0; fi\necho called >> '{}'\nexit 1\n",
                calls.display()
            ),
        )
        .unwrap();
        fs::set_permissions(bin.join("pi"), fs::Permissions::from_mode(0o755)).unwrap();
        Fixture {
            repo: GitFixture::new(),
            home,
        }
    }

    fn roots(&self) -> StorageRoots {
        let base = self.home.path().to_path_buf();
        StorageRoots::resolve(&|name| match name {
            "XDG_DATA_HOME" => Some(base.join("data").display().to_string()),
            "XDG_CONFIG_HOME" => Some(base.join("config").display().to_string()),
            "XDG_STATE_HOME" => Some(base.join("state").display().to_string()),
            "XDG_CACHE_HOME" => Some(base.join("cache").display().to_string()),
            _ => None,
        })
        .unwrap()
    }

    fn set_design_reviewer(&self) {
        let roots = self.roots();
        roots.create_all().unwrap();
        let mut store = Store::open(&roots.database()).unwrap();
        let root = fs::canonicalize(self.repo.root()).unwrap();
        let revision = store.project_settings(&root).unwrap().revision;
        let settings = ProjectSettings {
            design_reviewer: Some(ProjectAgent {
                harness: "pi".to_string(),
                model: "openai/gpt-6.1-sol".to_string(),
                effort: Some("medium".to_string()),
            }),
            ..ProjectSettings::default()
        };
        assert!(matches!(
            store
                .save_project_settings(&root, revision, &settings)
                .unwrap(),
            SaveOutcome::Saved { .. }
        ));
    }

    fn design_table(&self, start_command: &str) {
        fs::create_dir_all(self.repo.root().join(".orq")).unwrap();
        fs::write(
            self.repo.root().join(".orq/run.toml"),
            format!(
                "[design]\nstart_command = {start_command}\nmode = \"infer\"\n\n[design.viewports]\ndesktop = \"400x300\"\ntablet = \"300x400\"\nmobile = \"200x400\"\n"
            ),
        )
        .unwrap();
    }

    fn package(&self) {
        let references = self.repo.root().join("designs/home/references");
        fs::create_dir_all(&references).unwrap();
        for (name, width, height) in [
            ("desktop", 400, 300),
            ("tablet", 300, 400),
            ("mobile", 200, 400),
        ] {
            fs::write(references.join(format!("{name}.png")), png(width, height)).unwrap();
        }
        fs::write(self.repo.root().join("designs/home/DESIGN.md"), "# Home\n").unwrap();
    }

    fn review(&self, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.home.path().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        orq_test_support::sandboxed_command(env!("CARGO_BIN_EXE_orq"))
            .args(["design", "review", "--repo"])
            .arg(self.repo.root())
            .args(args)
            .env("PATH", path)
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("XDG_DATA_HOME", self.home.path().join("data"))
            .env("XDG_STATE_HOME", self.home.path().join("state"))
            .env("XDG_CACHE_HOME", self.home.path().join("cache"))
            .env("ORQ_CHROMIUM", self.home.path().join("no-chromium"))
            .env_remove("HERDR_ACTIVE_PANE_CWD")
            .output()
            .unwrap()
    }

    fn reviewer_called(&self) -> bool {
        self.home.path().join("pi-calls").exists()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn without_a_feature_it_prints_the_usage() {
    let f = Fixture::new();
    let output = f.review(&[]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("orq design review"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_project_without_a_design_table_is_refused() {
    let f = Fixture::new();
    f.set_design_reviewer();
    let output = f.review(&["--feature", "home"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("[design]"), "{}", stderr(&output));
    assert!(!f.reviewer_called());
}

#[test]
fn a_project_without_a_design_reviewer_is_refused() {
    let f = Fixture::new();
    f.design_table("[\"false\"]");
    let output = f.review(&["--feature", "home"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("set a design reviewer"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_page_that_cannot_be_captured_blocks_the_review_before_the_reviewer() {
    let f = Fixture::new();
    f.set_design_reviewer();
    f.design_table("[\"false\"]");
    f.package();
    let head = git(f.repo.root(), &["rev-parse", "HEAD"]);

    let output = f.review(&["--feature", "home"]);

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(stdout.contains("blocked"), "{stdout}");
    let report: PathBuf = stdout
        .lines()
        .find_map(|line| line.strip_prefix("report: "))
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("no report path in {stdout}"));
    let text = fs::read_to_string(&report).unwrap();
    assert!(
        text.contains("blocked") && text.contains("captured"),
        "{text}"
    );
    assert!(report.starts_with(f.roots().logs()), "{}", report.display());
    assert!(!f.reviewer_called());
    assert_eq!(git(f.repo.root(), &["rev-parse", "HEAD"]), head);
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

// ---------------------------------------------------------------- a valid PNG

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// A zlib stream of `n` zero bytes in stored (uncompressed) blocks.
fn zlib_zeros(n: usize) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut left = n;
    loop {
        let block = left.min(65_535);
        left -= block;
        out.push(if left == 0 { 1 } else { 0 });
        out.extend_from_slice(&(block as u16).to_le_bytes());
        out.extend_from_slice(&(!(block as u16)).to_le_bytes());
        out.extend(std::iter::repeat_n(0, block));
        if left == 0 {
            break;
        }
    }
    let adler = (((n % 65521) as u32) << 16) | 1;
    out.extend_from_slice(&adler.to_be_bytes());
    out
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(
        &mut out,
        b"IDAT",
        &zlib_zeros(height as usize * (1 + 3 * width as usize)),
    );
    png_chunk(&mut out, b"IEND", &[]);
    out
}
