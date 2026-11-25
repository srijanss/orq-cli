//! The design preparer of an infer-mode run can search the worktree: an asset the package calls existing and
//! the repository holds is written into the prepared DESIGN.md, not asked of a person.

use std::path::PathBuf;
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Feature `home`, frontend and in infer mode, whose package's DESIGN.md says the footer artwork is existing,
/// in a repository holding it at src/static/img/footer_background.webp. The page never starts, so the design
/// review is blocked before any reviewer.
struct Run {
    repo: GitFixture,
    home: PathBuf,
}

const FOOTER: &str = "src/static/img/footer_background.webp";

fn setup(tag: &str) -> Run {
    setup_with(tag, "home", None, true)
}

/// As [`setup`], the package in `designs/<package>/`, the feature's ledger `description`, and the footer artwork
/// in the repository only when `footer`.
fn setup_with(tag: &str, package: &str, description: Option<&str>, footer: bool) -> Run {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    let home = orq_test_support::process_temp_dir()
        .join(format!("orq-preparer-assets-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("bin")).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo.root())
            .args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", text(&out.stderr));
    };
    let write = |path: &str, bytes: &[u8]| {
        let path = repo.root().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    };
    write(
        ".orq/run.toml",
        b"[design]\nstart_command = [\"false\"]\nmode = \"infer\"\n\n[design.viewports]\ndesktop = \"400x300\"\n",
    );
    write(
        &format!("designs/{package}/DESIGN.md"),
        b"# Home\n\n### F01 Footer\n\nThe existing footer artwork, from the repository, behind the links.\n",
    );
    write(
        &format!("designs/{package}/references/desktop.png"),
        &png(400, 300),
    );
    if footer {
        write(FOOTER, b"RIFF\x00\x00\x00\x00WEBPVP8 fake");
    }
    let entry = LedgerEntry::new("home", "pending");
    let entry = match description {
        Some(description) => entry.description(description),
        None => entry,
    };
    write_ledger(repo.root(), &[entry]);
    git(&["add", "-A"]);
    git(&["commit", "--quiet", "-m", "design package"]);
    git(&["branch", "-M", "main"]);
    git(&["push", "--quiet", "origin", "main"]);
    git(&[
        "remote",
        "set-url",
        "origin",
        &format!("forge.example:{}", remote.display()),
    ]);
    let ssh = home.join("ssh");
    std::fs::write(
        &ssh,
        "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    Run { repo, home }
}

impl Run {
    fn command(&self) -> Command {
        let mut command = orq_test_support::sandboxed_command(env!("CARGO_BIN_EXE_orq"));
        command
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("XDG_STATE_HOME", self.home.join("state"))
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .env("ORQ_CHROMIUM", self.home.join("no-chromium"))
            .env("ORQ_FORGE", "gh")
            .env("GIT_SSH_COMMAND", self.home.join("ssh"))
            .env_remove("HERDR_ACTIVE_PANE_CWD")
            .env_remove("TDD_FEATURES_PATH");
        command
    }

    fn roots(&self) -> orq_core::storage::StorageRoots {
        orq_core::storage::StorageRoots::resolve(&|name| match name {
            "XDG_DATA_HOME" => Some(self.home.join("data").display().to_string()),
            "XDG_CONFIG_HOME" => Some(self.home.join("config").display().to_string()),
            "XDG_STATE_HOME" => Some(self.home.join("state").display().to_string()),
            "XDG_CACHE_HOME" => Some(self.home.join("cache").display().to_string()),
            _ => None,
        })
        .unwrap()
    }

    /// What `orq serve` answers for `path` under the project's API, `/api/projects/<id>/`.
    fn api(&self, path: &str) -> serde_json::Value {
        let roots = self.roots();
        let scan = self.repo.root().parent().unwrap().to_path_buf();
        let root = std::fs::canonicalize(self.repo.root()).unwrap();
        let project = orq_remote::projects::discover(&scan)
            .into_iter()
            .find(|p| p.path == root)
            .expect("the project is discovered");
        let provider = orq_remote::projects::StoreProjects::new(roots.clone(), scan);
        let config = orq_core::remote_inbox::RemoteConfig {
            token: "tok".into(),
        };
        let path = format!("/api/projects/{}/{path}", project.id);
        let request = orq_core::remote_inbox::Request {
            method: "GET",
            path: &path,
            peer: "127.0.0.1".parse().unwrap(),
            authorization: Some("Bearer tok"),
        };
        let reply = orq_core::remote_inbox::handle_with(
            &roots,
            &config,
            "server-project",
            "server",
            &request,
            &provider,
        );
        assert_eq!(reply.status, 200, "{path}: {}", reply.body);
        reply.body
    }

    /// The project's design agents are pi.
    fn design_reviewer(&self) {
        let roots = self.roots();
        roots.create_all().unwrap();
        let mut store = orq_core::store::Store::open(&roots.database()).unwrap();
        let root = std::fs::canonicalize(self.repo.root()).unwrap();
        let revision = store.project_settings(&root).unwrap().revision;
        let settings = orq_core::project_settings::ProjectSettings {
            design_reviewer: Some(orq_core::project_settings::ProjectAgent {
                harness: "pi".to_string(),
                model: "openai/gpt-6.1-sol".to_string(),
                effort: Some("medium".to_string()),
            }),
            ..Default::default()
        };
        store
            .save_project_settings(&root, revision, &settings)
            .unwrap();
        let kind = self
            .command()
            .args(["feature", "kind", "--repo"])
            .arg(self.repo.root())
            .args(["--id", "home", "frontend"])
            .output()
            .unwrap();
        assert!(kind.status.success(), "{}", text(&kind.stderr));
    }

    fn run(&self, agent: &FakeAgent, provider: &FakeProvider) -> Output {
        let bin = self.home.join("bin");
        let executable = |name: &str, script: &str| {
            std::fs::write(bin.join(name), script).unwrap();
            std::fs::set_permissions(
                bin.join(name),
                std::os::unix::fs::PermissionsExt::from_mode(0o755),
            )
            .unwrap();
        };
        executable(
            "claude",
            &format!("#!/bin/sh\nexec '{}' \"$@\"\n", agent.path().display()),
        );
        // As the design preparer it records its argv; given the search tools it finds the footer artwork in the
        // worktree it runs in and names it in DESIGN.md, needing nothing, else it asks a person for it.
        let pi = format!(
            "#!/bin/sh\nif [ \"$1\" = --list-models ]; then printf 'provider  model  context  max-out  thinking  images\\nopenai  gpt-6.1-sol  400K  128K  yes  yes\\n'; exit 0; fi\n\
             for a; do last=$a; done\n\
             case \"$last\" in *'You are the design preparer'*) ;; *) exit 1 ;; esac\n\
             printf '%s\\n' \"$@\" > \"$HOME/preparer-args\"\n\
             search=no; prev=; for a; do if [ \"$prev\" = --tools ] && [ \"$a\" = read,grep,find,ls ]; then search=yes; fi; prev=$a; done\n\
             if [ $search = yes ] && [ -f '{FOOTER}' ]; then\n\
             printf '{{\"files\":[{{\"path\":\"DESIGN.md\",\"content\":\"# Home\\\\n\\\\n## Existing assets\\\\n\\\\n- [measured] footer artwork: {FOOTER}\\\\n\"}},{{\"path\":\"RESPONSIVE.md\",\"content\":\"Stack.\"}},{{\"path\":\"tokens-mapping.md\",\"content\":\"none\"}}],\"conflicts\":[],\"inferred\":[],\"needed_files\":[]}}'\n\
             else\n\
             printf '{{\"conflicts\":[],\"inferred\":[],\"needed_files\":[{{\"path\":\"assets/footer_background.webp\",\"what\":\"the footer artwork\",\"why\":\"repository availability remains unverified\"}}]}}'\n\
             fi\n"
        );
        executable("pi", &pi);
        let mut dirs = vec![bin, provider.bin_dir()];
        dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        self.command()
            .args([
                "run",
                "--repo",
                self.repo.root().to_str().unwrap(),
                "--feature",
                "home",
                "--remote",
                "origin",
                "--target",
                "main",
                "--no-ci-check",
            ])
            .env("PATH", std::env::join_paths(dirs).unwrap())
            .output()
            .unwrap()
    }

    fn events(&self) -> Vec<serde_json::Value> {
        let mut events = Vec::new();
        for dir in std::fs::read_dir(self.home.join("state/orq/runs")).unwrap() {
            let text = std::fs::read_to_string(dir.unwrap().path().join("events.jsonl"))
                .unwrap_or_default();
            events.extend(
                text.lines()
                    .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()),
            );
        }
        events
    }

    fn logs(&self, run: &str) -> PathBuf {
        self.home.join("state/orq/logs").join(run)
    }
}

#[test]
fn a_preparer_that_finds_the_existing_footer_artwork_in_the_worktree_lets_the_run_carry_on_to_the_implementer(
) {
    let w = setup("found");
    w.design_reviewer();
    let agent = FakeAgent::builder()
        .write_file("index.html", "<h1>Home</h1>\n")
        .commit("feat: the home page")
        .build();
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .build();

    let out = w.run(&agent, &provider);

    let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
    let args = std::fs::read_to_string(w.home.join("preparer-args"))
        .unwrap_or_else(|e| panic!("the preparer never ran ({e}): {all}"));
    let args: Vec<&str> = args.lines().collect();
    assert!(
        args.windows(2)
            .any(|w| w == ["--tools", "read,grep,find,ls"]),
        "the preparer can search the worktree: {args:?}"
    );
    let events = w.events();
    assert!(
        !events.iter().any(|e| e["type"] == "design.files_needed"),
        "nothing is asked of a person: {all}\n{events:?}"
    );
    assert!(!all.contains("design_files_needed"), "{all}");
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "stage.completed" && e["stage"] == "implement_and_review"),
        "the run carries on to the implementer: {all}"
    );
    let run = events.iter().find_map(|e| e["run_id"].as_str()).unwrap();
    let prepared = std::fs::read_to_string(w.logs(run).join("design/prepared/DESIGN.md"))
        .unwrap_or_else(|e| panic!("{e}: {all}"));
    assert!(
        prepared.contains("Existing assets") && prepared.contains(FOOTER),
        "{prepared}"
    );
    let _ = std::fs::remove_dir_all(&w.home);
}

#[test]
fn a_run_of_a_feature_in_a_shared_package_that_stops_for_a_file_names_the_shared_packages_folder_on_the_web(
) {
    let w = setup_with(
        "shared",
        "egiftit-homepage",
        Some("The footer.\n\nDesign package: egiftit-homepage\nDesign components: F01\n"),
        false,
    );
    w.design_reviewer();
    let agent = FakeAgent::builder()
        .write_file("index.html", "<h1>Home</h1>\n")
        .commit("feat: the home page")
        .build();
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .build();

    let out = w.run(&agent, &provider);

    let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(
        all.contains("state: design_files_needed"),
        "the artwork is not in the repository: {all}"
    );
    let run = w
        .events()
        .iter()
        .find_map(|e| e["run_id"].as_str().map(str::to_string))
        .unwrap();
    let body = w.api(&format!("runs/{run}"));
    let needed = &body["run"]["design_files_needed"];
    assert_eq!(needed["package"], "egiftit-homepage", "{body}");
    assert_eq!(
        needed["files"][0]["path"], "designs/egiftit-homepage/assets/footer_background.webp",
        "{body}"
    );

    let features = w.api("features");
    let home = features["features"]
        .as_array()
        .and_then(|list| list.iter().find(|f| f["name"] == "home"))
        .unwrap_or_else(|| panic!("{features}"));
    assert_eq!(home["design_package"], "egiftit-homepage", "{features}");
    let _ = std::fs::remove_dir_all(&w.home);
}

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

/// A valid black PNG of `width` × `height`.
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
