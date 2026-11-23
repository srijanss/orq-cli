//! `mode = "infer"`: before the implementer, the design preparer writes what the frozen package
//! lacks into the run's `design/prepared/`. The preparer here is a fake that writes files and
//! answers as told; the step checks what it did and froze.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orq_orchestrator::{
    prepare_design, resume_design_package, start_design_package, AgentPreparer, DesignAgent,
    DesignManifest, DesignMode,
    DesignPackageOutcome, DesignPackageRun, DesignPrepareOutcome, DesignPreparer, DesignSettings,
    PackageLimits, PrepareRequest, Viewport,
};
use orq_test_support::{GitFixture, TempDir};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------- fixtures

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
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

/// A valid black PNG of `width` × `height`.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &zlib_zeros(height as usize * (1 + 3 * width as usize)));
    png_chunk(&mut out, b"IEND", &[]);
    out
}

fn settings(mode: DesignMode) -> DesignSettings {
    let viewports: BTreeMap<String, Viewport> =
        [("desktop", 400, 300), ("tablet", 300, 400), ("mobile", 200, 400)]
            .into_iter()
            .map(|(name, width, height)| (name.to_string(), Viewport { width, height }))
            .collect();
    DesignSettings {
        start_command: None,
        ready_path: "/".to_string(),
        ready_timeout: Duration::from_secs(5),
        max_correction_rounds: 3,
        verify_after_rounds: 2,
        mode,
        viewports,
    }
}

/// A main checkout with an infer-mode package of references only, its worktree and the run's logs.
struct Fixture {
    repo: GitFixture,
    worktree: PathBuf,
    logs: TempDir,
    settings: DesignSettings,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let repo = GitFixture::new();
        let worktree = repo.add_linked_worktree(tag);
        let f = Fixture { repo, worktree, logs: TempDir::new("orq-design-infer-logs"), settings: settings(DesignMode::Infer) };
        for (name, width, height) in [("desktop", 400, 300), ("tablet", 300, 400), ("mobile", 200, 400)] {
            f.put(&format!("references/{name}.png"), png(width, height));
        }
        f
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
            limits: PackageLimits::default(),
        }
    }

    fn freeze(&self) -> DesignManifest {
        match start_design_package(&self.run()) {
            DesignPackageOutcome::Frozen(manifest) => manifest,
            other => panic!("expected the package frozen, got {other:?}"),
        }
    }

    fn prepare(&self, manifest: &DesignManifest, preparer: &mut Fake) -> DesignPrepareOutcome {
        prepare_design(&self.run(), manifest, preparer)
    }
}

/// A preparer that runs `act` on the request, then answers; it counts its calls.
struct Fake {
    calls: u32,
    act: Box<dyn Fn(&PrepareRequest)>,
    answer: Box<dyn Fn(&PrepareRequest) -> serde_json::Value>,
}

impl Fake {
    fn new(act: impl Fn(&PrepareRequest) + 'static, answer: serde_json::Value) -> Fake {
        Fake { calls: 0, act: Box::new(act), answer: Box::new(move |_| answer.clone()) }
    }

    /// Answers what `answer` makes of the request.
    fn answering(self, answer: impl Fn(&PrepareRequest) -> serde_json::Value + 'static) -> Fake {
        Fake { answer: Box::new(answer), ..self }
    }

    /// Writes the three prepared files and answers with one inferred value and no needs.
    fn writing() -> Fake {
        Fake::new(write_prepared, clean_answer())
    }
}

impl DesignPreparer for Fake {
    fn prepare(&mut self, request: &PrepareRequest) -> Result<String, String> {
        self.calls += 1;
        (self.act)(request);
        Ok((self.answer)(request).to_string())
    }
}

fn write_prepared(request: &PrepareRequest) {
    fs::create_dir_all(request.prepared).unwrap();
    fs::write(request.prepared.join("DESIGN.md"), "# Checkout\nHeader 64px (inferred)\n").unwrap();
    fs::write(request.prepared.join("RESPONSIVE.md"), "Stack below 300px (proposed)\n").unwrap();
    fs::write(request.prepared.join("tokens-mapping.md"), "#123456 -> --brand (inferred)\n").unwrap();
}

fn clean_answer() -> serde_json::Value {
    serde_json::json!({
        "conflicts": [],
        "inferred": [{"value": "header height 64px", "label": "inferred", "source": "references/desktop.png"}],
        "needed_files": [],
    })
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

// ---------------------------------------------------------------- tests

#[test]
fn the_preparer_writes_the_prepared_files_which_are_frozen_hashed_and_given_to_the_implementer() {
    let f = Fixture::new("writes");
    let manifest = f.freeze();
    let mut preparer = Fake::writing();

    let outcome = f.prepare(&manifest, &mut preparer);

    let prepared = match outcome {
        DesignPrepareOutcome::Prepared(prepared) => prepared,
        other => panic!("expected the package prepared, got {other:?}"),
    };
    assert_eq!(preparer.calls, 1);
    let paths: Vec<&str> = prepared.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["DESIGN.md", "RESPONSIVE.md", "tokens-mapping.md"]);
    let design = f.logs.path().join("design");
    for file in &prepared.files {
        let bytes = read(&design.join("prepared").join(&file.path));
        assert_eq!(file.sha256, sha(&bytes), "{}", file.path);
        let copy = f.worktree.join(".orq-design-prepared").join(&file.path);
        assert_eq!(read(&copy), bytes, "the implementer's copy of {}", file.path);
        assert!(fs::metadata(&copy).unwrap().permissions().readonly(), "{}", file.path);
    }
    let saved: serde_json::Value = serde_json::from_slice(&read(&design.join("prepared.json"))).unwrap();
    assert_eq!(saved["files"].as_array().map(Vec::len), Some(3), "{saved}");
    let report = String::from_utf8(read(&prepared.report)).unwrap();
    assert!(report.contains("header height 64px") && report.contains("inferred"), "the inferred rules are labelled: {report}");
}

/// A preparer that needs `assets/hero.png` until the package has it.
fn needing_hero() -> Fake {
    Fake::new(write_prepared, clean_answer()).answering(|request| {
        let mut answer = clean_answer();
        if !request.package.join("assets/hero.png").is_file() {
            answer["needed_files"] = serde_json::json!([{
                "path": "assets/hero.png",
                "what": "the hero illustration",
                "why": "the desktop reference shows an illustration above the form",
            }]);
        }
        answer
    })
}

#[test]
fn a_missing_asset_pauses_with_the_list_and_adding_it_then_resuming_prepares_again_and_continues() {
    let f = Fixture::new("missing-asset");
    let manifest = f.freeze();
    let mut preparer = needing_hero();

    let needed = match f.prepare(&manifest, &mut preparer) {
        DesignPrepareOutcome::FilesNeeded(needed) => needed,
        other => panic!("expected design files needed, got {other:?}"),
    };

    assert_eq!(needed.feature, "checkout");
    let files: Vec<(&str, &str)> = needed.files.iter().map(|file| (file.what.as_str(), file.path.as_str())).collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0].1, "designs/checkout/assets/hero.png");
    assert!(files[0].0.contains("the hero illustration"), "{files:?}");
    assert!(files[0].0.contains("above the form"), "why it is needed: {files:?}");
    assert!(needed.message().contains("ChatGPT"), "{}", needed.message());
    let design = f.logs.path().join("design");
    assert!(!design.join("prepared.json").exists());
    assert!(!design.join("manifest.json").exists(), "a resume freezes the changed package again");
    assert!(!f.worktree.join(".orq-design-prepared").exists());

    f.put("assets/hero.png", png(10, 10));
    let refrozen = match resume_design_package(&f.run()) {
        DesignPackageOutcome::Frozen(manifest) => manifest,
        other => panic!("expected the changed package frozen again, got {other:?}"),
    };
    assert!(refrozen.files.iter().any(|file| file.path == "assets/hero.png"));
    assert!(matches!(f.prepare(&refrozen, &mut preparer), DesignPrepareOutcome::Prepared(_)));
    assert_eq!(preparer.calls, 2, "the preparer runs again on the new package");
}

#[test]
fn the_preparer_is_never_started_in_prepared_mode_or_for_a_package_with_no_reference() {
    let mut f = Fixture::new("not-started");
    f.settings.mode = DesignMode::Prepared;
    f.put("DESIGN.md", "Plain.\n");
    f.put("RESPONSIVE.md", "Stack.\n");
    f.put("UX.md", "One action.\n");
    f.put("tokens.css", ":root {}\n");
    let manifest = f.freeze();
    let mut preparer = Fake::writing();

    assert_eq!(f.prepare(&manifest, &mut preparer), DesignPrepareOutcome::Skipped);
    assert_eq!(preparer.calls, 0, "prepared mode makes no preparer call");
    assert!(!f.logs.path().join("design/prepared").exists());

    f.settings.mode = DesignMode::Infer;
    let mut no_reference = manifest.clone();
    no_reference.files.retain(|file| file.kind != "reference");
    let needed = match f.prepare(&no_reference, &mut preparer) {
        DesignPrepareOutcome::FilesNeeded(needed) => needed,
        other => panic!("expected design files needed, got {other:?}"),
    };
    let paths: Vec<&str> = needed.files.iter().map(|file| file.path.as_str()).collect();
    assert!(paths.contains(&"designs/checkout/references/desktop.png"), "{paths:?}");
    assert_eq!(preparer.calls, 0, "no preparer without a reference");
}

#[test]
fn an_unresolved_conflict_pauses_asking_for_a_decision_and_a_resolved_one_is_reported() {
    let f = Fixture::new("conflicts");
    let manifest = f.freeze();
    let conflict = |resolved: bool| {
        let mut answer = clean_answer();
        answer["conflicts"] = serde_json::json!([{
            "description": "the header is blue on desktop and green on mobile",
            "sources": ["references/desktop.png", "references/mobile.png"],
            "followed": if resolved { serde_json::json!("references/desktop.png") } else { serde_json::Value::Null },
            "resolved": resolved,
        }]);
        answer
    };

    let needed = match f.prepare(&manifest, &mut Fake::new(write_prepared, conflict(false))) {
        DesignPrepareOutcome::FilesNeeded(needed) => needed,
        other => panic!("expected design files needed, got {other:?}"),
    };
    assert_eq!(needed.files.len(), 1, "{needed:?}");
    assert!(needed.files[0].what.contains("blue on desktop"), "{needed:?}");
    assert!(needed.files[0].path.starts_with("designs/checkout/"), "{needed:?}");

    let manifest = f.freeze();
    let prepared = match f.prepare(&manifest, &mut Fake::new(write_prepared, conflict(true))) {
        DesignPrepareOutcome::Prepared(prepared) => prepared,
        other => panic!("expected the package prepared, got {other:?}"),
    };
    let report = String::from_utf8(read(&prepared.report)).unwrap();
    assert!(report.contains("blue on desktop") && report.contains("followed references/desktop.png"), "{report}");
}

#[test]
fn a_preparer_that_makes_an_image_or_answers_badly_fails_the_step() {
    let f = Fixture::new("bad-output");
    let manifest = f.freeze();
    let image = Fake::new(
        |request| {
            write_prepared(request);
            fs::write(request.prepared.join("hero.png"), png(10, 10)).unwrap();
        },
        clean_answer(),
    );
    let mut escaping = clean_answer();
    escaping["needed_files"] = serde_json::json!([{"path": "../../etc/passwd", "what": "x", "why": "y"}]);
    let cases = [
        ("image", image, "never an image"),
        ("not json", Fake::writing().answering(|_| serde_json::json!("all done")), "not valid"),
        ("escaping path", Fake::new(write_prepared, escaping), "not a plain path"),
    ];
    for (tag, mut preparer, expected) in cases {
        let why = failed(f.prepare(&manifest, &mut preparer));
        assert!(why.contains(expected), "{tag}: {why}");
        assert!(!f.logs.path().join("design/prepared.json").exists(), "{tag}");
    }
}

/// A fake harness program: records its arguments next to itself, says something on stderr, then
/// runs `body`.
fn fake_program(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let program = dir.join(name);
    fs::write(
        &program,
        format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n--\\n' \"$a\"; done > \"$(dirname \"$0\")/args\"\necho 'from the agent' >&2\n{body}\n"),
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    program
}

/// Runs the preparer, again while a test running alongside still holds the fresh script open
/// (a fork between its write and close makes exec fail as busy).
fn prepare_retrying(preparer: &mut AgentPreparer, request: &PrepareRequest) -> Result<String, String> {
    for _ in 0..50 {
        match preparer.prepare(request) {
            Err(why) if why.contains("ExecutableFileBusy") => std::thread::sleep(Duration::from_millis(20)),
            other => return other,
        }
    }
    preparer.prepare(request)
}

#[test]
fn the_agent_preparer_runs_a_read_only_harness_and_orq_writes_the_files_it_answers_into_the_prepared_folder() {
    let f = Fixture::new("agent");
    let manifest = f.freeze();
    let bin = TempDir::new("orq-design-infer-agent");
    let prepared = f.logs.path().join("design/prepared");
    fs::create_dir_all(&prepared).unwrap();
    let package = f.logs.path().join("design/package");
    let request = PrepareRequest { feature: "checkout", package: &package, prepared: &prepared, manifest: &manifest };
    let full = r##"{"files":[{"path":"DESIGN.md","content":"# Checkout\n"},{"path":"notes/tokens-mapping.md","content":"#123456 -> --brand\n"}],"conflicts":[],"inferred":[],"needed_files":[]}"##;
    let answer = r#"{"conflicts":[],"inferred":[],"needed_files":[]}"#;
    let log = f.logs.path().join("design/preparer.log");
    let answering = fake_program(bin.path(), "answering", &format!("printf '%s' '{full}'"));
    let escaping = fake_program(
        bin.path(),
        "escaping",
        r#"printf '%s' '{"files":[{"path":"../escape.md","content":"x"}],"conflicts":[],"inferred":[],"needed_files":[]}'"#,
    );
    let imaging = fake_program(
        bin.path(),
        "imaging",
        r#"printf '%s' '{"files":[{"path":"hero.png","content":"x"}],"conflicts":[],"inferred":[],"needed_files":[]}'"#,
    );
    let failing = fake_program(bin.path(), "failing", "exit 3");
    let slow = fake_program(bin.path(), "slow", "sleep 30");

    for harness in ["pi", "claude"] {
        let agent = DesignAgent {
            program: answering.clone(),
            harness: harness.to_string(),
            model: "openai/gpt-6.1-sol".to_string(),
            thinking: Some("medium".to_string()),
        };
        let mut preparer = AgentPreparer {
            agent: &agent,
            worktree: &f.worktree,
            skill: "SKILL TEXT of design-prepare",
            log: log.clone(),
            timeout: Duration::from_secs(20),
        };
        let given = prepare_retrying(&mut preparer, &request).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&given).unwrap(),
            serde_json::from_str::<serde_json::Value>(answer).unwrap(),
            "{harness}: the answer without its files"
        );
        assert_eq!(read(&prepared.join("DESIGN.md")), b"# Checkout\n", "{harness}");
        assert_eq!(read(&prepared.join("notes/tokens-mapping.md")), b"#123456 -> --brand\n", "{harness}");
        let args = String::from_utf8(read(&bin.path().join("args"))).unwrap();
        let args: Vec<&str> = args.split("\n--\n").collect();
        let prompt = args[args.len() - 2];
        assert!(prompt.contains("SKILL TEXT of design-prepare"), "{harness}: {prompt}");
        assert!(prompt.contains(&package.display().to_string()), "{harness}: {prompt}");
        assert!(prompt.contains("needed_files") && prompt.contains("\"files\""), "{harness}: the answer's shape is given: {prompt}");
        assert!(args.contains(&"openai/gpt-6.1-sol"), "{harness}: {args:?}");
        // The session gets a read tool only: it can write nowhere, orq writes its files.
        let tools = if harness == "pi" { "read" } else { "Read" };
        assert!(args.windows(2).any(|w| w == ["--tools", tools]), "{args:?}");
        let flags = &args[..args.len() - 2];
        assert!(!flags.iter().any(|a| a.contains("write") || a.contains("Write") || *a == "--add-dir"), "{flags:?}");
        assert!(String::from_utf8(read(&log)).unwrap().contains("from the agent"), "{harness}");
    }

    for (tag, program, expected) in [("escaping", &escaping, "not a plain"), ("image", &imaging, "never an image")] {
        let agent = DesignAgent { program: program.clone(), harness: "pi".to_string(), model: "m".to_string(), thinking: None };
        let mut preparer =
            AgentPreparer { agent: &agent, worktree: &f.worktree, skill: "s", log: log.clone(), timeout: Duration::from_secs(20) };
        let why = prepare_retrying(&mut preparer, &request).unwrap_err();
        assert!(why.contains(expected), "{tag}: {why}");
    }
    assert!(!f.logs.path().join("design/escape.md").exists(), "nothing is written outside the prepared folder");
    assert!(!prepared.join("hero.png").exists());

    let agent = |program: &PathBuf| DesignAgent {
        program: program.clone(),
        harness: "pi".to_string(),
        model: "m".to_string(),
        thinking: None,
    };
    let failing = agent(&failing);
    let slow = agent(&slow);
    for (tag, agent, timeout, expected) in [
        ("fails", &failing, Duration::from_secs(20), "failed"),
        ("too slow", &slow, Duration::from_millis(300), "did not answer"),
    ] {
        let mut preparer =
            AgentPreparer { agent, worktree: &f.worktree, skill: "s", log: log.clone(), timeout };
        let why = prepare_retrying(&mut preparer, &request).unwrap_err();
        assert!(why.contains(expected), "{tag}: {why}");
    }
}

#[test]
fn a_preparation_that_leaves_a_document_missing_fails_and_one_the_package_already_has_is_not_needed() {
    let f = Fixture::new("incomplete");
    let manifest = f.freeze();
    let only_design = |request: &PrepareRequest| fs::write(request.prepared.join("DESIGN.md"), "# Checkout\n").unwrap();
    let cases: [(&str, Fake, &[&str]); 3] = [
        ("nothing", Fake::new(|_| {}, clean_answer()), &["DESIGN.md", "RESPONSIVE.md", "token mapping"]),
        ("unrelated", Fake::new(|r| fs::write(r.prepared.join("notes.txt"), "hi\n").unwrap(), clean_answer()), &["DESIGN.md"]),
        ("partial", Fake::new(only_design, clean_answer()), &["RESPONSIVE.md", "token mapping"]),
    ];
    for (tag, mut preparer, missing) in cases {
        let why = failed(f.prepare(&manifest, &mut preparer));
        for doc in missing {
            assert!(why.contains(doc), "{tag}: {why}");
        }
        assert!(!f.logs.path().join("design/prepared.json").exists(), "{tag}");
    }

    // A package with its own documents and tokens needs nothing prepared.
    f.put("DESIGN.md", "# Checkout\n");
    f.put("RESPONSIVE.md", "Stack.\n");
    f.put("tokens.css", ":root {}\n");
    let manifest = f.freeze();
    match f.prepare(&manifest, &mut Fake::new(|_| {}, clean_answer())) {
        DesignPrepareOutcome::Prepared(prepared) => assert!(prepared.files.is_empty()),
        other => panic!("expected the package prepared, got {other:?}"),
    }
}

#[test]
fn a_preparation_frozen_for_the_same_package_is_kept_without_a_preparer_call_and_a_changed_one_is_prepared_again() {
    let f = Fixture::new("kept");
    let manifest = f.freeze();
    let first = match f.prepare(&manifest, &mut Fake::writing()) {
        DesignPrepareOutcome::Prepared(prepared) => prepared,
        other => panic!("expected the package prepared, got {other:?}"),
    };
    // The resumed run's worktree has no copy yet.
    fs::remove_dir_all(f.worktree.join(".orq-design-prepared")).unwrap();

    let mut again = Fake::writing();
    assert_eq!(f.prepare(&manifest, &mut again), DesignPrepareOutcome::Prepared(first.clone()));
    assert_eq!(again.calls, 0, "the frozen preparation is kept");
    assert_eq!(read(&f.worktree.join(".orq-design-prepared/DESIGN.md")), read(&f.logs.path().join("design/prepared/DESIGN.md")));

    // A prepared file that no longer matches its hash is prepared again.
    fs::write(f.logs.path().join("design/prepared/DESIGN.md"), "# Tampered\n").unwrap();
    let mut tampered = Fake::writing();
    assert!(matches!(f.prepare(&manifest, &mut tampered), DesignPrepareOutcome::Prepared(_)));
    assert_eq!(tampered.calls, 1);

    // A changed package is prepared again.
    f.put("UX.md", "One action.\n");
    let changed = f.freeze();
    let mut fresh = Fake::writing();
    match f.prepare(&changed, &mut fresh) {
        DesignPrepareOutcome::Prepared(prepared) => assert_eq!(prepared.package_sha256, changed.package_sha256),
        other => panic!("expected the package prepared, got {other:?}"),
    }
    assert_eq!(fresh.calls, 1);
}

fn failed(outcome: DesignPrepareOutcome) -> String {
    match outcome {
        DesignPrepareOutcome::Failed(why) => why,
        other => panic!("expected the step failed, got {other:?}"),
    }
}

#[test]
fn a_preparer_that_changes_a_package_file_fails_the_step_and_nothing_is_frozen() {
    // Each change gets (the main checkout's package, the worktree, the request).
    let changes: [(&str, fn(&Path, &Path, &PrepareRequest)); 3] = [
        ("frozen", |_, _, request| fs::write(request.package.join("references/desktop.png"), png(400, 301)).unwrap()),
        ("main", |package, _, _| fs::write(package.join("DESIGN.md"), "# Mine now\n").unwrap()),
        ("copy", |_, worktree, _| {
            let copy = worktree.join(".orq-design/references/tablet.png");
            let mut permissions = fs::metadata(&copy).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            fs::set_permissions(&copy, permissions).unwrap();
            fs::write(copy, b"scribble").unwrap();
        }),
    ];
    for (tag, change) in changes {
        let f = Fixture::new(&format!("changes-{tag}"));
        let manifest = f.freeze();
        let (package, worktree) = (f.package(), f.worktree.clone());
        let mut preparer = Fake::new(
            move |request| {
                write_prepared(request);
                change(&package, &worktree, request);
            },
            clean_answer(),
        );

        let why = failed(f.prepare(&manifest, &mut preparer));

        assert!(why.contains("the design preparer changed the design package"), "{tag}: {why}");
        assert!(!f.logs.path().join("design/prepared.json").exists(), "{tag}");
        assert!(!f.worktree.join(".orq-design-prepared").exists(), "{tag}");
    }
}
