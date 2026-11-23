use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orq_orchestrator::{
    resume_design_package, start_design_package, DesignManifest, DesignMode, DesignPackageOutcome,
    DesignPackageRun, DesignSettings, PackageLimits, Viewport,
};
use orq_test_support::{GitFixture, TempDir};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------- image fixtures

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

/// Writes a deflate stream's bits: values least significant bit first, Huffman codes most
/// significant bit first.
struct BitWriter {
    out: Vec<u8>,
    buffer: u32,
    count: u32,
}

impl BitWriter {
    fn bits(&mut self, value: u32, count: u32) {
        for i in 0..count {
            self.buffer |= ((value >> i) & 1) << self.count;
            self.count += 1;
            if self.count == 8 {
                self.out.push(self.buffer as u8);
                self.buffer = 0;
                self.count = 0;
            }
        }
    }

    fn code(&mut self, code: u32, length: u32) {
        for i in (0..length).rev() {
            self.bits((code >> i) & 1, 1);
        }
    }
}

/// A zlib stream of `n` zero bytes, compressed with deflate's fixed Huffman codes: a literal
/// zero, then 258-byte copies of the byte before.
fn zlib_zeros(n: u64) -> Vec<u8> {
    let mut w = BitWriter {
        out: vec![0x78, 0x01],
        buffer: 0,
        count: 0,
    };
    w.bits(1, 1); // the last block
    w.bits(1, 2); // fixed codes
    let mut left = n;
    if left > 0 {
        w.code(0x30, 8); // literal 0
        left -= 1;
    }
    while left >= 258 {
        w.code(0xC5, 8); // length 258
        w.code(0, 5); // distance 1
        left -= 258;
    }
    for _ in 0..left {
        w.code(0x30, 8);
    }
    w.code(0, 7); // end of block
    if w.count > 0 {
        w.out.push(w.buffer as u8);
    }
    let adler = (((n % 65521) as u32) << 16) | 1;
    w.out.extend_from_slice(&adler.to_be_bytes());
    w.out
}

fn png_with(width: u32, height: u32, idat: &[u8]) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", idat);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

/// A black RGB PNG of `width` x `height`: every row's filter byte and pixels are whole.
fn png(width: u32, height: u32) -> Vec<u8> {
    png_with(
        width,
        height,
        &zlib_zeros(height as u64 * (1 + 3 * width as u64)),
    )
}

fn jpeg_segment(out: &mut Vec<u8>, marker: u8, body: &[u8]) {
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&(body.len() as u16 + 2).to_be_bytes());
    out.extend_from_slice(body);
}

/// A baseline JPEG of `width` x `height` with its quantization and Huffman tables, a scan and
/// entropy-coded data.
fn jpeg(width: u16, height: u16) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8];
    let mut dqt = vec![0];
    dqt.extend([1; 64]);
    jpeg_segment(&mut out, 0xDB, &dqt);
    let mut sof = vec![8];
    sof.extend_from_slice(&height.to_be_bytes());
    sof.extend_from_slice(&width.to_be_bytes());
    sof.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 0, 3, 0x11, 0]);
    jpeg_segment(&mut out, 0xC0, &sof);
    let mut dht = vec![0x00, 1];
    dht.extend([0; 15]);
    dht.push(0);
    jpeg_segment(&mut out, 0xC4, &dht);
    jpeg_segment(&mut out, 0xDA, &[3, 1, 0x00, 2, 0x00, 3, 0x00, 0, 63, 0]);
    out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

fn webp(width: u32, height: u32) -> Vec<u8> {
    let bits: u32 = (width - 1) | ((height - 1) << 14);
    let mut chunk = vec![0x2F];
    chunk.extend_from_slice(&bits.to_le_bytes());
    chunk.push(0);
    let mut body = b"WEBP".to_vec();
    body.extend_from_slice(b"VP8L");
    body.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
    body.extend_from_slice(&chunk);
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

const SAFE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><defs><circle id="c" r="4"/></defs><use href="#c"/><image href="data:image/png;base64,AAAA"/></svg>"##;

// ---------------------------------------------------------------- run fixtures

fn settings(mode: DesignMode) -> DesignSettings {
    let viewports: BTreeMap<String, Viewport> = [
        ("desktop", 1440, 900),
        ("tablet", 768, 1024),
        ("mobile", 390, 844),
    ]
    .into_iter()
    .map(|(name, width, height)| (name.to_string(), Viewport { width, height }))
    .collect();
    DesignSettings {
        start_command: Some(vec!["npm".into(), "run".into(), "dev".into()]),
        ready_path: "/".into(),
        ready_timeout: Duration::from_secs(60),
        max_correction_rounds: 3,
        verify_after_rounds: 2,
        mode,
        viewports,
    }
}

/// A main checkout, its run worktree and the run's log folder.
struct Fixture {
    repo: GitFixture,
    worktree: PathBuf,
    logs: TempDir,
    settings: DesignSettings,
    limits: PackageLimits,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let repo = GitFixture::new();
        let worktree = repo.add_linked_worktree(tag);
        Fixture {
            repo,
            worktree,
            logs: TempDir::new("orq-design-logs"),
            settings: settings(DesignMode::Prepared),
            limits: PackageLimits::default(),
        }
    }

    fn package(&self) -> PathBuf {
        self.repo.root().join("designs/checkout")
    }

    fn put(&self, path: &str, bytes: impl AsRef<[u8]>) {
        let path = self.package().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn run(&self) -> DesignPackageRun<'_> {
        DesignPackageRun {
            main_checkout: self.repo.root(),
            feature: "checkout",
            settings: &self.settings,
            run_logs: self.logs.path(),
            worktree: &self.worktree,
            limits: self.limits,
        }
    }

    fn start(&self) -> DesignPackageOutcome {
        start_design_package(&self.run())
    }

    fn resume(&self) -> DesignPackageOutcome {
        resume_design_package(&self.run())
    }

    /// Every reference and document a prepared run needs.
    fn full_package(&self) {
        self.put("references/desktop.png", png(1440, 900));
        self.put("references/tablet.jpg", jpeg(768, 1024));
        self.put("references/mobile.webp", webp(390, 844));
        self.put("assets/icons/logo.svg", SAFE_SVG);
        self.put("assets/fonts/brand.woff2", b"wOF2\x00\x01\x00\x00rest");
        self.put("DESIGN.md", "Use assets/icons/logo.svg in the header.\n");
        self.put("RESPONSIVE.md", "Stack below 768px.\n");
        self.put("UX.md", "One primary action.\n");
        self.put("tokens.css", ":root { --brand: #123456; }\n");
    }
}

fn frozen(outcome: DesignPackageOutcome) -> DesignManifest {
    match outcome {
        DesignPackageOutcome::Frozen(manifest) => manifest,
        other => panic!("expected the package frozen, got {other:?}"),
    }
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

// ---------------------------------------------------------------- tests

#[test]
fn a_full_package_is_frozen_into_the_run_logs_with_every_file_in_its_manifest() {
    let f = Fixture::new("full");
    f.full_package();

    let manifest = frozen(f.start());

    let paths: Vec<&str> = manifest
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(
        paths,
        [
            "DESIGN.md",
            "RESPONSIVE.md",
            "UX.md",
            "assets/fonts/brand.woff2",
            "assets/icons/logo.svg",
            "references/desktop.png",
            "references/mobile.webp",
            "references/tablet.jpg",
            "tokens.css",
        ]
    );
    for file in &manifest.files {
        let bytes = read(&f.package().join(&file.path));
        assert_eq!(file.sha256, sha(&bytes), "{}", file.path);
        assert_eq!(file.size, bytes.len() as u64, "{}", file.path);
        assert_eq!(
            read(&f.logs.path().join("design/package").join(&file.path)),
            bytes
        );
    }
    let file = |path: &str| {
        manifest
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap()
    };
    let desktop = file("references/desktop.png");
    assert_eq!(
        (desktop.kind.as_str(), desktop.width, desktop.height),
        ("reference", Some(1440), Some(900))
    );
    assert_eq!(
        desktop
            .viewport
            .as_ref()
            .map(|v| (v.name.as_str(), v.width, v.height)),
        Some(("desktop", 1440, 900))
    );
    assert_eq!(file("references/tablet.jpg").width, Some(768));
    assert_eq!(file("references/mobile.webp").height, Some(844));
    assert_eq!(file("assets/icons/logo.svg").kind, "asset");
    assert_eq!(file("DESIGN.md").kind, "document");
    assert_eq!(file("tokens.css").kind, "tokens");
    assert!(manifest.unassigned.is_empty());
    assert!(manifest.missing.viewports.is_empty() && manifest.missing.documents.is_empty());
    assert!(manifest.missing.assets.is_empty());

    let saved: DesignManifest =
        serde_json::from_slice(&read(&f.logs.path().join("design/manifest.json"))).unwrap();
    assert_eq!(saved, manifest);
}

#[test]
fn a_reference_named_with_its_size_is_that_viewport_and_any_other_is_unassigned() {
    let mut f = Fixture::new("sizes");
    f.settings.mode = DesignMode::Infer;
    f.put("references/desktop-1280x800.png", png(2560, 1600));
    f.put("references/tablet.png", png(768, 1024));
    f.put("references/mobile-375x667.jpg", jpeg(375, 667));
    f.put("references/moodboard.png", png(800, 600));

    let manifest = frozen(f.start());

    let viewport = |path: &str| {
        let file = manifest
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        file.viewport
            .as_ref()
            .map(|v| (v.name.clone(), v.width, v.height))
    };
    assert_eq!(
        viewport("references/desktop-1280x800.png"),
        Some(("desktop".into(), 1280, 800))
    );
    assert_eq!(
        viewport("references/mobile-375x667.jpg"),
        Some(("mobile".into(), 375, 667))
    );
    assert_eq!(
        viewport("references/tablet.png"),
        Some(("tablet".into(), 768, 1024))
    );
    assert_eq!(viewport("references/moodboard.png"), None);
    assert_eq!(manifest.unassigned, ["references/moodboard.png"]);
    assert!(manifest.missing.viewports.is_empty());
}

fn refused(outcome: DesignPackageOutcome) -> Vec<String> {
    match outcome {
        DesignPackageOutcome::Refused(problems) => problems,
        other => panic!("expected the package refused, got {other:?}"),
    }
}

fn mentions(problems: &[String], words: &[&str]) -> bool {
    problems.iter().any(|p| words.iter().all(|w| p.contains(w)))
}

#[test]
fn symlinks_in_the_package_are_refused_and_every_one_is_listed() {
    let f = Fixture::new("symlink");
    f.full_package();
    let outside = f.logs.path().join("secret.png");
    fs::write(&outside, png(1440, 900)).unwrap();
    fs::remove_file(f.package().join("references/desktop.png")).unwrap();
    std::os::unix::fs::symlink(&outside, f.package().join("references/desktop.png")).unwrap();
    std::os::unix::fs::symlink(f.logs.path(), f.package().join("assets/elsewhere")).unwrap();

    let problems = refused(f.start());

    assert!(
        mentions(&problems, &["references/desktop.png", "symlink"]),
        "{problems:?}"
    );
    assert!(
        mentions(&problems, &["assets/elsewhere", "symlink"]),
        "{problems:?}"
    );
    assert!(!f.logs.path().join("design/manifest.json").exists());
    assert!(!f.worktree.join(".orq-design").exists());
}

#[test]
fn a_package_folder_that_is_a_symlink_is_refused() {
    let f = Fixture::new("symlinked-folder");
    let elsewhere = f.logs.path().join("pkg");
    fs::create_dir_all(elsewhere.join("references")).unwrap();
    fs::write(elsewhere.join("references/desktop.png"), png(1440, 900)).unwrap();
    fs::create_dir_all(f.repo.root().join("designs")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, f.package()).unwrap();

    let problems = refused(f.start());

    assert!(
        mentions(&problems, &["designs/checkout", "symlink"]),
        "{problems:?}"
    );
}

#[test]
fn a_feature_whose_package_path_would_leave_the_designs_folder_is_refused() {
    let f = Fixture::new("dotdot");
    fs::create_dir_all(f.repo.root().join("designs")).unwrap();
    fs::create_dir_all(f.repo.root().join("src/references")).unwrap();
    fs::write(
        f.repo.root().join("src/references/desktop.png"),
        png(1440, 900),
    )
    .unwrap();

    for feature in ["../src", "a/../../src", "/tmp", ""] {
        let run = DesignPackageRun { feature, ..f.run() };
        let problems = refused(start_design_package(&run));
        assert!(
            mentions(&problems, &["not a usable design package name"]),
            "{feature}: {problems:?}"
        );
    }
    assert!(!f.logs.path().join("design").exists());
}

#[test]
fn a_document_naming_a_path_with_dotdot_is_refused() {
    let f = Fixture::new("doc-dotdot");
    f.full_package();
    f.put("DESIGN.md", "The logo is assets/../../../secret.svg\n");

    let problems = refused(f.start());

    assert!(
        mentions(
            &problems,
            &["DESIGN.md", "assets/../../../secret.svg", ".."]
        ),
        "{problems:?}"
    );
}

#[test]
fn images_that_do_not_decode_are_refused_and_every_one_is_listed() {
    let f = Fixture::new("undecodable");
    f.full_package();
    let mut broken_crc = png(1440, 900);
    let last = broken_crc.len() - 13;
    broken_crc[last] ^= 0xFF;
    f.put("references/desktop.png", broken_crc);
    f.put("references/tablet.jpg", b"not a jpeg at all");
    let mut short_webp = webp(390, 844);
    short_webp.truncate(short_webp.len() - 3);
    f.put("references/mobile.webp", short_webp);
    f.put("assets/hero.png", &png(10, 10)[..30]);
    f.put("references/print.pdf", b"hello");
    f.put("references/wide.gif", b"GIF89a");
    f.put("assets/fonts/body.woff", b"nope");

    let problems = refused(f.start());

    for path in [
        "references/desktop.png",
        "references/tablet.jpg",
        "references/mobile.webp",
        "assets/hero.png",
        "references/print.pdf",
        "references/wide.gif",
        "assets/fonts/body.woff",
    ] {
        assert!(
            mentions(&problems, &[path]),
            "{path} not listed: {problems:?}"
        );
    }
    assert_eq!(problems.len(), 7, "{problems:?}");
}

#[test]
fn images_with_whole_headers_but_unusable_pixel_data_are_refused() {
    let f = Fixture::new("unusable-pixels");
    f.full_package();
    // Every chunk's CRC is right, but the data holds one row of 900.
    f.put(
        "references/desktop.png",
        png_with(
            1440,
            900,
            &[0x78, 0x01, 0x01, 0x01, 0x00, 0xFE, 0xFF, 0x00, 0x00, 0x01, 0x00, 0x01],
        ),
    );
    // Not a zlib stream at all.
    f.put("assets/hero.png", png_with(10, 10, b"pixels go here"));
    // Whole rows, but one too many.
    f.put("assets/tall.png", png_with(10, 10, &zlib_zeros(11 * 31)));
    // A frame header, then the end: no tables, no scan.
    let mut header_only = vec![0xFF, 0xD8];
    jpeg_segment(
        &mut header_only,
        0xC0,
        &[8, 0x04, 0x00, 0x03, 0x00, 1, 1, 0x11, 0],
    );
    header_only.extend_from_slice(&[0xFF, 0xD9]);
    f.put("references/tablet.jpg", header_only);
    // A VP8X header with no image after it.
    let mut vp8x = b"WEBP".to_vec();
    vp8x.extend_from_slice(b"VP8X");
    vp8x.extend_from_slice(&10u32.to_le_bytes());
    vp8x.extend_from_slice(&[0, 0, 0, 0, 0x85, 0x01, 0x00, 0x4B, 0x03, 0x00]);
    let mut webp_header = b"RIFF".to_vec();
    webp_header.extend_from_slice(&(vp8x.len() as u32).to_le_bytes());
    webp_header.extend_from_slice(&vp8x);
    f.put("references/mobile.webp", webp_header);

    let problems = refused(f.start());

    for path in [
        "references/desktop.png",
        "assets/hero.png",
        "assets/tall.png",
        "references/tablet.jpg",
        "references/mobile.webp",
    ] {
        assert!(
            mentions(&problems, &[path, "does not decode"]),
            "{path} not refused: {problems:?}"
        );
    }
    assert_eq!(problems.len(), 5, "{problems:?}");
    assert!(!f.logs.path().join("design/manifest.json").exists());
}

#[test]
fn pngs_of_stored_blocks_or_interlaced_rows_decode_when_their_data_is_whole() {
    let f = Fixture::new("png-variants");
    f.full_package();
    // 10 rows of 1 + 3 * 10 bytes, in one stored block.
    let mut stored = vec![0x78, 0x01, 0x01];
    stored.extend_from_slice(&310u16.to_le_bytes());
    stored.extend_from_slice(&(!310u16).to_le_bytes());
    stored.extend([0; 310]);
    stored.extend_from_slice(&((310u32 << 16) | 1).to_be_bytes());
    f.put("assets/stored.png", png_with(10, 10, &stored));
    // Adam7's seven passes over 10 x 10 RGB hold 320 bytes.
    let mut interlaced = png_with(10, 10, &zlib_zeros(320));
    interlaced[8 + 8 + 12] = 1;
    let crc = crc32(&interlaced[12..29]);
    interlaced[29..33].copy_from_slice(&crc.to_be_bytes());
    f.put("assets/interlaced.png", interlaced);
    // Its filter bytes say a filter PNG doesn't have.
    let mut stored_bad = stored.clone();
    stored_bad[7] = 9;
    f.put("assets/unknown-filter.png", png_with(10, 10, &stored_bad));

    let problems = refused(f.start());

    assert!(
        mentions(&problems, &["assets/unknown-filter.png", "does not decode"]),
        "{problems:?}"
    );
    assert_eq!(problems.len(), 1, "{problems:?}");
}

#[test]
fn a_pdf_reference_is_kept_with_its_viewport_and_no_pixel_size() {
    let f = Fixture::new("pdf");
    f.full_package();
    f.put(
        "references/desktop.pdf",
        b"%PDF-1.7\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF\n",
    );
    fs::remove_file(f.package().join("references/desktop.png")).unwrap();

    let manifest = frozen(f.start());

    let pdf = manifest
        .files
        .iter()
        .find(|file| file.path == "references/desktop.pdf")
        .unwrap();
    assert_eq!((pdf.width, pdf.height), (None, None));
    assert_eq!(
        pdf.viewport.as_ref().map(|v| v.name.as_str()),
        Some("desktop")
    );
}

#[test]
fn unsafe_svgs_are_refused_and_safe_ones_kept() {
    let f = Fixture::new("svg");
    f.full_package();
    let svg = |body: &str| format!(r#"<svg xmlns="http://www.w3.org/2000/svg">{body}</svg>"#);
    f.put("assets/script.svg", svg("<SCRIPT>alert(1)</SCRIPT>"));
    f.put(
        "assets/onload.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" onLoad = "alert(1)"/>"#,
    );
    f.put(
        "assets/foreign.svg",
        svg("<foreignObject><div/></foreignObject>"),
    );
    f.put(
        "assets/external.svg",
        svg(r#"<image href="https://example.com/x.png"/>"#),
    );
    f.put(
        "assets/xlink.svg",
        svg(r#"<use xlink:href='other.svg#a'/>"#),
    );
    f.put(
        "assets/data-svg.svg",
        svg(r#"<image href="data:image/svg+xml;base64,AAAA"/>"#),
    );
    f.put("assets/ok.svg", svg(r##"<!-- hover on-state, see https://x --><g id="one"/><use href="#one"/><path d="M0 0" fill="url(#one)" opacity="1"/>"##));

    let problems = refused(f.start());

    for path in [
        "assets/script.svg",
        "assets/onload.svg",
        "assets/foreign.svg",
        "assets/external.svg",
        "assets/xlink.svg",
        "assets/data-svg.svg",
    ] {
        assert!(
            mentions(&problems, &[path]),
            "{path} not refused: {problems:?}"
        );
    }
    assert!(!mentions(&problems, &["assets/ok.svg"]), "{problems:?}");
    assert!(
        !mentions(&problems, &["assets/icons/logo.svg"]),
        "{problems:?}"
    );
}

#[test]
fn svgs_loading_external_resources_through_css_or_attributes_are_refused() {
    let f = Fixture::new("svg-css");
    f.full_package();
    let svg = |body: &str| format!(r#"<svg xmlns="http://www.w3.org/2000/svg">{body}</svg>"#);
    let refused_bodies = [
        ("import", r#"<style>@import "https://example.com/a.css";</style>"#),
        (
            "cdata",
            "<style><![CDATA[ rect { fill: url(https://example.com/p.svg#g) } ]]></style>",
        ),
        (
            "fill",
            r#"<rect fill="url(https://example.com/p.svg#paint)"/>"#,
        ),
        (
            "style-attr",
            r#"<rect style="filter: url( 'other.svg#f' )"/>"#,
        ),
        (
            "entity",
            r#"<rect fill="&#117;rl(https://example.com/p.svg#paint)"/>"#,
        ),
        ("escape", r"<style>rect { fill: u\72l(https://x/p.svg#a) }</style>"),
        ("src", r#"<image src="https://example.com/x.png"/>"#),
    ];
    for (name, body) in refused_bodies {
        f.put(&format!("assets/{name}.svg"), svg(body));
    }
    f.put(
        "assets/stylesheet.svg",
        r#"<?xml-stylesheet href="https://example.com/a.css"?><svg xmlns="http://www.w3.org/2000/svg"/>"#,
    );
    f.put(
        "assets/xxe.svg",
        r#"<!DOCTYPE svg [<!ENTITY x SYSTEM "https://example.com/x">]><svg xmlns="http://www.w3.org/2000/svg">&x;</svg>"#,
    );
    f.put(
        "assets/local.svg",
        svg(r##"<style>rect { fill: url(#g) }</style><rect fill="url( '#g' )" style="clip-path:url(#c)"/>"##),
    );

    let problems = refused(f.start());

    for (name, _) in refused_bodies {
        let path = format!("assets/{name}.svg");
        assert!(mentions(&problems, &[&path]), "{path} not refused: {problems:?}");
    }
    for path in ["assets/stylesheet.svg", "assets/xxe.svg"] {
        assert!(mentions(&problems, &[path]), "{path} not refused: {problems:?}");
    }
    assert!(!mentions(&problems, &["assets/local.svg"]), "{problems:?}");
}

#[test]
fn character_references_in_an_svg_attribute_cannot_hide_the_attributes_after_it() {
    let f = Fixture::new("svg-references");
    f.full_package();
    f.put(
        "assets/hidden-onload.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" data-x="&quot;&gt;&lt;!--" onload="alert(1)"/>"#,
    );
    f.put(
        "assets/hidden-href.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg"><g data-x="&quot;&gt;&lt;!--"/><image href="https://example.com/x.png"/></svg>"#,
    );
    f.put(
        "assets/quoted.svg",
        r##"<svg xmlns="http://www.w3.org/2000/svg"><title>a &lt;b&gt; &amp; c</title><g data-x="&quot;&gt;"/><use href="#g"/></svg>"##,
    );

    let problems = refused(f.start());

    assert!(
        mentions(&problems, &["assets/hidden-onload.svg", "onload"]),
        "{problems:?}"
    );
    assert!(
        mentions(&problems, &["assets/hidden-href.svg", "href"]),
        "{problems:?}"
    );
    assert!(!mentions(&problems, &["assets/quoted.svg"]), "{problems:?}");
}

#[test]
fn svg_comments_hide_no_attributes_however_many_there_are() {
    let f = Fixture::new("svg-comments");
    f.full_package();
    let comments = "<!-- onload=\"x\" -->".repeat(5000);
    f.put(
        "assets/commented.svg",
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg">{comments}<g id="a"/></svg>"#),
    );
    f.put(
        "assets/after.svg",
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg">{comments}<g onclick="x"/></svg>"#),
    );
    f.put(
        "assets/unclosed.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg"><!-- <g onclick="x"/></svg>"#,
    );

    let problems = refused(f.start());

    assert!(mentions(&problems, &["assets/after.svg", "onclick"]), "{problems:?}");
    assert!(!mentions(&problems, &["assets/commented.svg"]), "{problems:?}");
    assert!(!mentions(&problems, &["assets/unclosed.svg"]), "{problems:?}");
}

#[test]
fn a_package_over_its_file_count_or_size_limit_is_refused() {
    let mut f = Fixture::new("limits");
    f.full_package();
    assert_eq!(
        PackageLimits::default(),
        PackageLimits {
            max_total_bytes: 200 * 1024 * 1024,
            max_files: 2000
        }
    );

    f.limits = PackageLimits {
        max_files: 5,
        ..PackageLimits::default()
    };
    let problems = refused(f.start());
    assert!(mentions(&problems, &["more than 5 files"]), "{problems:?}");

    f.limits = PackageLimits {
        max_total_bytes: 100,
        ..PackageLimits::default()
    };
    let problems = refused(f.start());
    assert!(mentions(&problems, &["100 bytes"]), "{problems:?}");

    f.limits = PackageLimits {
        max_files: 9,
        max_total_bytes: 100_000,
    };
    frozen(f.start());
}

#[test]
fn a_reference_whose_width_does_not_match_its_viewport_is_frozen_with_a_warning() {
    let f = Fixture::new("aspect");
    f.full_package();
    f.put("references/desktop.png", png(1000, 900));
    f.put("references/tablet.jpg", jpeg(1536, 4000));

    let manifest = frozen(f.start());

    assert_eq!(manifest.warnings.len(), 1, "{:?}", manifest.warnings);
    let warning = &manifest.warnings[0];
    assert!(
        warning.contains("references/desktop.png") && warning.contains("1440"),
        "{warning}"
    );
}

fn needed(outcome: DesignPackageOutcome) -> Vec<String> {
    match outcome {
        DesignPackageOutcome::FilesNeeded(request) => {
            assert_eq!(request.feature, "checkout");
            request.files.iter().map(|file| file.path.clone()).collect()
        }
        other => panic!("expected design files needed, got {other:?}"),
    }
}

const EVERY_DOCUMENT: [&str; 4] = [
    "designs/checkout/DESIGN.md",
    "designs/checkout/RESPONSIVE.md",
    "designs/checkout/UX.md",
    "designs/checkout/tokens.css",
];

#[test]
fn no_package_folder_pauses_with_every_file_asked_for_and_the_folder_layout_created() {
    let f = Fixture::new("no-folder");

    let outcome = f.start();

    let DesignPackageOutcome::FilesNeeded(request) = &outcome else {
        panic!("expected design files needed, got {outcome:?}");
    };
    let message = request.message();
    assert!(
        message.contains("designs/checkout/references/desktop.png"),
        "{message}"
    );
    assert!(
        message.contains("ChatGPT") || message.contains("make"),
        "{message}"
    );
    let mut expected = vec![
        "designs/checkout/references/desktop.png",
        "designs/checkout/references/mobile.png",
        "designs/checkout/references/tablet.png",
    ];
    expected.extend(EVERY_DOCUMENT);
    assert_eq!(needed(outcome), expected);
    assert!(f.package().join("references").is_dir());
    assert!(f.package().join("assets").is_dir());
    assert!(!f.logs.path().join("design/manifest.json").exists());
    assert!(!f.worktree.join(".orq-design").exists());
}

#[test]
fn a_package_of_documents_only_pauses_asking_for_every_reference_even_in_infer_mode() {
    let mut f = Fixture::new("documents-only");
    f.settings.mode = DesignMode::Infer;
    f.put("DESIGN.md", "Plain.\n");
    f.put("RESPONSIVE.md", "Stack.\n");
    f.put("UX.md", "One action.\n");
    f.put("tokens.json", "{}\n");

    assert_eq!(
        needed(f.start()),
        [
            "designs/checkout/references/desktop.png",
            "designs/checkout/references/mobile.png",
            "designs/checkout/references/tablet.png",
        ]
    );
    assert!(f.package().join("references").is_dir());
}

#[test]
fn a_package_changed_in_the_main_checkout_pauses_the_resumed_run() {
    let f = Fixture::new("changed");
    f.full_package();
    let started = frozen(f.start());

    assert_eq!(
        frozen(f.resume()),
        started,
        "an unchanged package resumes as frozen"
    );

    f.put("UX.md", "Two primary actions.\n");
    match f.resume() {
        DesignPackageOutcome::Changed(message) => {
            assert!(
                message.contains("the design package changed since this run started"),
                "{message}"
            )
        }
        other => panic!("expected the run paused on a changed package, got {other:?}"),
    }
    // Neither copy is silently used: the frozen one stays as it was.
    assert_eq!(
        read(&f.logs.path().join("design/package/UX.md")),
        b"One primary action.\n"
    );
    let saved: DesignManifest =
        serde_json::from_slice(&read(&f.logs.path().join("design/manifest.json"))).unwrap();
    assert_eq!(saved.package_sha256, started.package_sha256);

    fs::remove_dir_all(f.package()).unwrap();
    assert!(matches!(f.resume(), DesignPackageOutcome::Changed(_)));
}

#[test]
fn desktop_only_references_in_prepared_mode_ask_for_tablet_mobile_and_named_assets_until_added() {
    let f = Fixture::new("desktop-only");
    f.put("references/desktop.png", png(1440, 900));
    f.put(
        "DESIGN.md",
        "Header: `assets/icons/logo.svg`; hero (assets/hero.webp).\n",
    );
    f.put("RESPONSIVE.md", "Stack.\n");
    f.put("UX.md", "One action.\n");
    f.put("tokens.css", ":root{}\n");

    let asked = vec![
        "designs/checkout/references/mobile.png".to_string(),
        "designs/checkout/references/tablet.png".to_string(),
        "designs/checkout/assets/icons/logo.svg".to_string(),
        "designs/checkout/assets/hero.webp".to_string(),
    ];
    assert_eq!(needed(f.start()), asked);
    assert_eq!(
        needed(f.resume()),
        asked,
        "resuming without the files stays paused with the same list"
    );

    f.put("references/tablet.png", png(768, 1024));
    f.put("references/mobile-390x844.png", png(390, 844));
    f.put("assets/icons/logo.svg", SAFE_SVG);
    assert_eq!(needed(f.resume()), ["designs/checkout/assets/hero.webp"]);

    f.put("assets/hero.webp", webp(1200, 600));
    let manifest = frozen(f.resume());
    assert!(manifest.missing.assets.is_empty() && manifest.missing.viewports.is_empty());
    assert!(f.logs.path().join("design/manifest.json").exists());
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn the_implementer_gets_a_read_only_copy_in_the_worktree_that_git_never_sees() {
    let f = Fixture::new("worktree-copy");
    f.full_package();

    frozen(f.start());

    let copy = f.worktree.join(".orq-design/references/desktop.png");
    assert_eq!(read(&copy), png(1440, 900));
    assert!(fs::metadata(&copy).unwrap().permissions().readonly());
    assert_eq!(
        read(&f.worktree.join(".orq-design/DESIGN.md")),
        read(&f.package().join("DESIGN.md"))
    );
    assert_eq!(
        git_out(
            &f.worktree,
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        ""
    );

    // Starting again replaces the read-only copy.
    f.put("UX.md", "Two primary actions.\n");
    frozen(f.start());
    assert_eq!(
        read(&f.worktree.join(".orq-design/UX.md")),
        b"Two primary actions.\n"
    );
}

#[test]
fn a_freeze_whose_worktree_copy_fails_publishes_no_manifest_so_resume_starts_again() {
    let f = Fixture::new("partial-freeze");
    f.full_package();
    let not_a_worktree = f.logs.path().join("not-a-worktree");
    fs::write(&not_a_worktree, "a file, not a folder").unwrap();
    let run = DesignPackageRun {
        worktree: &not_a_worktree,
        ..f.run()
    };

    refused(start_design_package(&run));
    assert!(!f.logs.path().join("design/manifest.json").exists());

    // The run resumes in its real worktree: the package is frozen afresh, copy included.
    frozen(f.resume());
    assert_eq!(
        read(&f.worktree.join(".orq-design/DESIGN.md")),
        read(&f.package().join("DESIGN.md"))
    );
}

#[test]
fn resuming_a_frozen_run_restores_the_worktree_copy() {
    let f = Fixture::new("restore-copy");
    f.full_package();
    frozen(f.start());
    fs::remove_file(f.worktree.join(".orq-design/DESIGN.md")).unwrap();
    fs::remove_file(f.worktree.join(".orq-design/references/desktop.png")).unwrap();

    frozen(f.resume());
    assert_eq!(
        read(&f.worktree.join(".orq-design/DESIGN.md")),
        read(&f.package().join("DESIGN.md"))
    );
    let copy = f.worktree.join(".orq-design/references/desktop.png");
    assert_eq!(read(&copy), png(1440, 900));
    assert!(fs::metadata(&copy).unwrap().permissions().readonly());
}
