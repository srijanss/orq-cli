//! `orq serve install` puts `orq serve` under a systemd user unit, `orq-serve.service`, that restarts it after a
//! crash or a reboot; `orq serve status` says how the unit is doing and `orq serve uninstall` takes it away.
//!
//! `XDG_CONFIG_HOME` is a temp dir, so the unit lands under it, and stand-in `systemctl`, `loginctl` and
//! `journalctl` on the PATH note each call and answer with what the test gives them. The orq under test is a
//! build under `target/`, so the unit runs the installed orq, a stand-in named by `ORQ_INSTALLED_BINARY`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

/// How install and status look at the unit.
const SHOW: &str = "systemctl --user show orq-serve.service -p UnitFileState -p ActiveState -p NRestarts\n";
const TOKEN: &str = "install-token-0123456789abcdef01234567";

/// A fresh root with a repository, a token file, an installed orq and stand-in system tools.
struct World {
    root: PathBuf,
    repo: PathBuf,
    installed: PathBuf,
}

impl World {
    fn new(tag: &str) -> World {
        let root = orq_test_support::process_temp_dir().join(format!("orq-serve-install-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // A space in each path, so the unit has to quote them.
        let repo = root.join("my repo");
        fs::create_dir_all(&repo).unwrap();
        assert!(Command::new("git").arg("-C").arg(&repo).args(["init", "-q", "-b", "main"]).output().unwrap().status.success());
        let config = root.join("config/orq");
        fs::create_dir_all(&config).unwrap();
        let token = config.join("remote-token");
        fs::write(&token, TOKEN).unwrap();
        fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
        let bin = root.join("installed bin");
        fs::create_dir_all(&bin).unwrap();
        let installed = bin.join("orq");
        fs::write(&installed, "#!/bin/sh\necho 'orq 9.9.9 (fffffff 2030-01-01T00:00:00Z)'\n").unwrap();
        fs::set_permissions(&installed, fs::Permissions::from_mode(0o755)).unwrap();
        let world = World { root, repo, installed };
        world.answer("systemctl", "");
        world.answer("loginctl", "echo Linger=yes");
        world.answer("journalctl", "");
        world
    }

    /// Makes the stand-in `tool` note each call and then run `answer`, a shell command.
    fn answer(&self, tool: &str, answer: &str) {
        let fake = self.root.join("fakebin");
        fs::create_dir_all(&fake).unwrap();
        let path = fake.join(tool);
        let calls = self.root.join("calls");
        fs::write(&path, format!("#!/bin/sh\necho \"{tool} $*\" >> '{}'\n{answer}\n", calls.display())).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Makes the stand-in `systemctl` say the unit is enabled and running.
    fn healthy(&self) {
        self.answer("systemctl", "case \"$*\" in *show*) printf 'UnitFileState=enabled\\nActiveState=active\\nNRestarts=0\\n';; esac");
    }

    /// Each `systemctl` call the stand-in saw, one per line.
    fn systemctl_calls(&self) -> String {
        self.calls().lines().filter(|line| line.starts_with("systemctl ")).map(|line| format!("{line}\n")).collect()
    }

    /// Each call the stand-in tools saw, one per line.
    fn calls(&self) -> String {
        fs::read_to_string(self.root.join("calls")).unwrap_or_default()
    }

    fn forget_calls(&self) {
        let _ = fs::remove_file(self.root.join("calls"));
    }

    fn unit_path(&self) -> PathBuf {
        self.root.join("config/systemd/user/orq-serve.service")
    }

    fn unit(&self) -> String {
        fs::read_to_string(self.unit_path()).unwrap()
    }

    fn command(&self) -> Command {
        let mut command = orq_test_support::sandboxed_command(std::path::Path::new(env!("CARGO_BIN_EXE_orq")));
        for (name, dir) in [("XDG_STATE_HOME", "state"), ("XDG_DATA_HOME", "data"), ("XDG_CONFIG_HOME", "config"), ("HOME", "home")] {
            command.env(name, self.root.join(dir));
        }
        let path = format!("{}:{}", self.root.join("fakebin").display(), std::env::var("PATH").unwrap_or_default());
        command.env("PATH", path).env("USER", "tester").env("ORQ_INSTALLED_BINARY", &self.installed).env_remove("INVOCATION_ID").env_remove("NOTIFY_SOCKET");
        command
    }

    /// `orq serve <args>` in this world.
    fn serve(&self, args: &[&str]) -> Output {
        self.command().arg("serve").args(args).output().unwrap()
    }

    fn install(&self, listen: &str) -> Output {
        let repo = self.repo.display().to_string();
        self.serve(&["install", "--repo", &repo, "--listen", listen])
    }

    /// `orq serve install` run from a shell whose PATH is the stand-in tools, then `entries`, then the system's.
    fn install_with_path(&self, entries: &[&str], env: &[(&str, &str)]) -> Output {
        let repo = self.repo.display().to_string();
        let mut command = self.command();
        command.env("PATH", self.shell_path(entries)).envs(env.iter().copied());
        command.args(["serve", "install", "--repo", &repo, "--listen", "127.0.0.1:7999"]).output().unwrap()
    }

    /// The stand-in tools, then `entries`, then the system's own `/usr/bin:/bin`.
    fn shell_path(&self, entries: &[&str]) -> String {
        let mut all = vec![self.root.join("fakebin").display().to_string()];
        all.extend(entries.iter().map(|entry| entry.to_string()));
        all.extend(["/usr/bin".to_string(), "/bin".to_string()]);
        all.join(":")
    }

    /// The `Environment="PATH=..."` line of the written unit, unquoted, `None` when it has none.
    fn unit_path_line(&self) -> Option<String> {
        self.unit().lines().find_map(|line| line.strip_prefix("Environment=\"PATH=").and_then(|rest| rest.strip_suffix('"')).map(String::from))
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn ok(output: &Output) -> String {
    assert!(output.status.success(), "stdout: {}\nstderr: {}", text(&output.stdout), text(&output.stderr));
    text(&output.stdout)
}

#[test]
fn install_writes_a_unit_that_restarts_the_installed_orq_and_enables_it() {
    let world = World::new("unit");
    ok(&world.install("127.0.0.1:7999"));
    let unit = world.unit();
    let exec = format!("ExecStart=\"{}\" \"serve\" \"--repo\" \"{}\" \"--listen\" \"127.0.0.1:7999\"\n", world.installed.display(), world.repo.display());
    assert!(unit.contains(&exec), "ExecStart runs the installed orq, never a target/ build, with each word quoted:\n{unit}");
    for line in ["Restart=always\n", "RestartSec=5\n", "StartLimitIntervalSec=600\n", "StartLimitBurst=10\n", "StandardOutput=journal\n", "KillSignal=SIGTERM\n", "TimeoutStopSec=30\n", "WantedBy=default.target\n"] {
        assert!(unit.contains(line), "{line:?} missing:\n{unit}");
    }
    assert_eq!(world.calls(), "systemctl --user daemon-reload\nsystemctl --user enable --now orq-serve.service\nloginctl show-user tester -p Linger\n");
}

#[test]
fn the_unit_keeps_the_path_install_was_run_with_so_runs_find_their_tools() {
    let world = World::new("path");
    let out = ok(&world.install_with_path(&["/home/me/.local/bin", "/home/me/.cargo/bin"], &[]));
    let path = world.shell_path(&["/home/me/.local/bin", "/home/me/.cargo/bin"]);
    assert_eq!(world.unit_path_line().as_deref(), Some(path.as_str()), "{}", world.unit());
    assert!(out.contains(&format!("PATH: {path}\n")), "install prints the PATH it wrote: {out}");
}

#[test]
fn an_fnm_shell_folder_is_swapped_for_the_default_alias_and_other_runtime_folders_are_dropped() {
    let world = World::new("fnm");
    let run = world.root.join("run");
    let fnm = world.root.join("fnm");
    let default_bin = fnm.join("aliases/default/bin");
    fs::create_dir_all(&default_bin).unwrap();
    let shell = run.join("fnm_multishells/123_456/bin").display().to_string();
    let other = run.join("other/bin").display().to_string();
    let env = [("XDG_RUNTIME_DIR", run.to_str().unwrap()), ("FNM_DIR", fnm.to_str().unwrap())];
    let out = ok(&world.install_with_path(&[&shell, "/home/me/.local/bin", &other], &env));
    let expected = world.shell_path(&[default_bin.to_str().unwrap(), "/home/me/.local/bin"]);
    assert_eq!(world.unit_path_line().as_deref(), Some(expected.as_str()), "{}", world.unit());
    assert!(out.contains(&format!("PATH: {expected}\n")), "{out}");
    assert!(out.contains(&other), "install names the entry it dropped: {out}");
}

#[test]
fn install_warns_when_the_projects_harness_is_not_on_the_services_path_but_still_installs() {
    let world = World::new("noharness");
    let out = ok(&world.install_with_path(&["/home/me/.local/bin"], &[]));
    let path = world.shell_path(&["/home/me/.local/bin"]);
    assert!(out.contains(&format!("runs will fail: claude is not on the PATH the service gets ({path})")), "{out}");
    assert!(world.unit_path().exists(), "the install goes ahead");
    let tools = world.root.join("tools");
    fs::create_dir_all(&tools).unwrap();
    fs::write(tools.join("claude"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(tools.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    let out = ok(&world.install_with_path(&[tools.to_str().unwrap()], &[]));
    assert!(!out.contains("runs will fail: claude"), "claude is found now: {out}");
}

#[test]
fn a_folder_named_twice_is_kept_once_and_percent_signs_in_the_path_are_escaped() {
    let world = World::new("dedupe");
    ok(&world.install_with_path(&["/home/me/.local/bin", "", "/opt/100% $HOME", "/home/me/.local/bin", "/usr/bin"], &[]));
    let expected = world.shell_path(&["/home/me/.local/bin", "/opt/100%% $HOME"]);
    assert_eq!(world.unit_path_line().as_deref(), Some(expected.as_str()), "{}", world.unit());
}

#[test]
fn relative_path_entries_are_left_out_since_they_would_mean_the_services_folder_not_the_shells() {
    let world = World::new("relative");
    let out = ok(&world.install_with_path(&[".", "node_modules/.bin", "/home/me/.local/bin"], &[]));
    let expected = world.shell_path(&["/home/me/.local/bin"]);
    assert_eq!(world.unit_path_line().as_deref(), Some(expected.as_str()), "{}", world.unit());
    assert!(out.contains("left out of the service's PATH (relative): node_modules/.bin"), "{out}");
    assert!(out.contains("left out of the service's PATH (relative): .\n"), "{out}");
}

#[test]
fn a_relative_fnm_dir_never_puts_a_relative_folder_on_the_services_path() {
    let world = World::new("relfnm");
    fs::create_dir_all(world.root.join("fnm/aliases/default/bin")).unwrap();
    let run = world.root.join("run");
    let shell = run.join("fnm_multishells/123_456/bin").display().to_string();
    let repo = world.repo.display().to_string();
    let mut command = world.command();
    command.current_dir(&world.root).env("PATH", world.shell_path(&[&shell])).env("XDG_RUNTIME_DIR", &run).env("FNM_DIR", "fnm");
    ok(&command.args(["serve", "install", "--repo", &repo, "--listen", "127.0.0.1:7999"]).output().unwrap());
    assert_eq!(world.unit_path_line().as_deref(), Some(world.shell_path(&[]).as_str()), "{}", world.unit());
}

#[test]
fn without_an_fnm_default_alias_the_shell_folder_is_dropped_too() {
    let world = World::new("nofnm");
    let run = world.root.join("run");
    let shell = run.join("fnm_multishells/123_456/bin").display().to_string();
    let env = [("XDG_RUNTIME_DIR", run.to_str().unwrap()), ("FNM_DIR", "/nonexistent/fnm")];
    let out = ok(&world.install_with_path(&[&shell, "/home/me/.local/bin"], &env));
    let expected = world.shell_path(&["/home/me/.local/bin"]);
    assert_eq!(world.unit_path_line().as_deref(), Some(expected.as_str()), "{}", world.unit());
    assert!(out.contains(&shell), "install names the entry it dropped: {out}");
}

#[test]
fn without_linger_install_names_the_command_to_turn_it_on_and_never_runs_it() {
    let world = World::new("linger");
    world.answer("loginctl", "echo Linger=no");
    let out = ok(&world.install("127.0.0.1:7999"));
    assert!(out.contains("loginctl enable-linger tester"), "{out}");
    assert!(!world.calls().contains("enable-linger"), "{}", world.calls());
    let with = World::new("linger-on");
    let out = ok(&with.install("127.0.0.1:7999"));
    assert!(!out.contains("enable-linger"), "linger already on needs no hint: {out}");
}

#[test]
fn an_identical_reinstall_changes_nothing_and_new_flags_rewrite_the_unit_and_restart_it() {
    let world = World::new("again");
    ok(&world.install("127.0.0.1:7999"));
    let first = world.unit();
    world.healthy();
    world.forget_calls();
    let out = ok(&world.install("127.0.0.1:7999"));
    assert!(out.contains("already installed"), "{out}");
    assert_eq!(world.unit(), first);
    assert_eq!(world.systemctl_calls(), SHOW, "an identical install of a running unit only looks at it");
    world.forget_calls();
    ok(&world.install("127.0.0.1:7998"));
    assert!(world.unit().contains("\"--listen\" \"127.0.0.1:7998\""), "{}", world.unit());
    assert!(world.calls().starts_with("systemctl --user daemon-reload\nsystemctl --user enable orq-serve.service\nsystemctl --user restart orq-serve.service\n"), "{}", world.calls());
}

/// An `orq serve` started by hand, killed and reaped when the test ends.
struct HandStarted(std::process::Child);

impl Drop for HandStarted {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl World {
    /// Starts `orq serve` by hand, outside any unit, and returns it with the address it listens on.
    fn serve_by_hand(&self) -> (HandStarted, String) {
        self.serve_with(&[])
    }

    /// Starts `orq serve` with `env` set, and returns it with the address it listens on.
    fn serve_with(&self, env: &[(&str, &str)]) -> (HandStarted, String) {
        use std::io::BufRead;
        let mut command = self.command();
        command.envs(env.iter().copied());
        let mut child = command.arg("serve").arg("--repo").arg(&self.repo).args(["--listen", "127.0.0.1:0"]).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let running = HandStarted(child);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(addr) = line.split("http://").nth(1) {
                    let _ = tx.send(addr.trim().to_string());
                }
            }
        });
        let addr = rx.recv_timeout(std::time::Duration::from_secs(15)).expect("the server did not announce its address");
        // The server names itself in its file just after the address line.
        let file = self.root.join(format!("state/orq/serve/{}.json", running.0.id()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !fs::read_to_string(&file).is_ok_and(|text| text.contains(&addr)) {
            assert!(std::time::Instant::now() < deadline, "the server never wrote {}", file.display());
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        (running, addr)
    }
}

#[test]
fn install_refuses_while_an_orq_serve_started_by_hand_listens_there_and_names_its_pid() {
    let world = World::new("busy");
    let (server, addr) = world.serve_by_hand();
    let out = world.install(&addr);
    assert!(!out.status.success(), "{}", text(&out.stdout));
    let err = text(&out.stderr);
    assert!(err.contains(&format!("pid {}", server.0.id())) && err.contains(&addr), "{err}");
    assert!(!world.unit_path().exists(), "nothing is written before the address is free");
    assert!(!world.calls().contains("systemctl"), "{}", world.calls());
    // It was not stopped.
    let mut server = server;
    assert!(server.0.try_wait().unwrap().is_none(), "install never stops the server");
    // Any address on the same port is the same one to a server listening on all of them.
    let port = addr.rsplit(':').next().unwrap();
    assert!(!world.install(&format!("0.0.0.0:{port}")).status.success());
    assert!(world.install("127.0.0.1:7999").status.success(), "another port is free");
}

#[test]
fn uninstall_stops_and_disables_the_unit_and_removes_its_file_but_not_logs_or_data() {
    let world = World::new("uninstall");
    ok(&world.install("127.0.0.1:7999"));
    let state = world.root.join("state/orq");
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("serve.log"), "kept").unwrap();
    world.forget_calls();
    let out = ok(&world.serve(&["uninstall"]));
    assert!(!world.unit_path().exists(), "{out}");
    assert_eq!(world.calls(), "systemctl --user disable --now orq-serve.service\nsystemctl --user daemon-reload\n");
    assert_eq!(fs::read_to_string(state.join("serve.log")).unwrap(), "kept");
    assert!(world.root.join("config/orq/remote-token").exists());
    world.forget_calls();
    let again = world.serve(&["uninstall"]);
    assert!(again.status.success() && text(&again.stdout).contains("not installed"), "{}", text(&again.stdout));
    assert_eq!(world.calls(), "", "nothing to stop when there is no unit");
}

#[test]
fn status_says_whether_the_unit_is_installed_enabled_and_active_its_restarts_linger_binary_and_last_journal_lines() {
    let world = World::new("status");
    let out = ok(&world.serve(&["status"]));
    assert!(out.contains("unit: not installed"), "{out}");
    assert!(out.contains("linger: yes"), "{out}");
    ok(&world.install("127.0.0.1:7999"));
    world.answer("systemctl", "case \"$*\" in *show*) printf 'UnitFileState=enabled\\nActiveState=active\\nNRestarts=3\\n';; esac");
    world.answer("loginctl", "echo Linger=no");
    world.answer("journalctl", "printf 'one\\ntwo\\nthree\\nfour\\nfive\\n'");
    world.forget_calls();
    let out = ok(&world.serve(&["status"]));
    for line in [
        format!("unit: {}", world.unit_path().display()),
        "enabled: yes".into(),
        "active: yes".into(),
        "restarts: 3".into(),
        "linger: no".into(),
        format!("binary: {}", world.installed.display()),
        "one\n".into(),
        "five\n".into(),
    ] {
        assert!(out.contains(&line), "{line:?} missing:\n{out}");
    }
    let calls = world.calls();
    assert!(calls.contains("systemctl --user show orq-serve.service -p UnitFileState -p ActiveState -p NRestarts\n"), "{calls}");
    assert!(calls.contains("journalctl --user -u orq-serve.service -n 5 --no-pager\n"), "{calls}");
}

#[test]
fn status_shows_the_path_systemd_gives_the_service_so_a_drop_in_wins_and_warns_when_a_harness_is_missing() {
    let world = World::new("statuspath");
    let path = world.shell_path(&["/home/me/.local/bin"]);
    ok(&world.install_with_path(&["/home/me/.local/bin"], &[]));
    let out = ok(&world.serve(&["status"]));
    assert!(out.contains(&format!("path: {path}\n")), "without systemd's word, the unit file's PATH: {out}");
    world.answer("systemctl", "case \"$*\" in *Environment*) echo 'Environment=ORQ_SYSTEMD_UNIT=orq-serve.service \"PATH=/drop in/bin:/usr/bin\"';; esac");
    world.forget_calls();
    let out = ok(&world.serve(&["status"]));
    assert!(world.calls().contains("systemctl --user show orq-serve.service -p Environment\n"), "{}", world.calls());
    assert!(out.contains("path: /drop in/bin:/usr/bin\n"), "a drop-in's PATH wins: {out}");
    assert!(out.contains("runs will fail: claude is not on the PATH the service gets (/drop in/bin:/usr/bin)"), "{out}");
}

#[test]
fn status_decodes_the_c_escapes_systemd_shows_in_the_path() {
    let world = World::new("statusescapes");
    ok(&world.install_with_path(&[], &[]));
    world.answer("systemctl", r#"case "$*" in *Environment*) printf '%s\n' 'Environment=ORQ_SYSTEMD_UNIT=orq-serve.service "PATH=/tools \"x\"\\y\ttab\x41\nline:/usr/bin"';; esac"#);
    let out = ok(&world.serve(&["status"]));
    assert!(out.contains("path: /tools \"x\"\\y\ttabA\nline:/usr/bin\n"), "{out}");
}

/// `GET /api/settings` with the bearer token; the JSON body.
fn settings(addr: &str) -> serde_json::Value {
    use std::io::{Read, Write};
    let wait = std::time::Duration::from_secs(15);
    let mut stream = std::net::TcpStream::connect_timeout(&addr.parse().unwrap(), wait).unwrap();
    stream.set_read_timeout(Some(wait)).unwrap();
    write!(stream, "GET /api/settings HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {TOKEN}\r\nConnection: close\r\n\r\n").unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    serde_json::from_str(reply.split_once("\r\n\r\n").unwrap().1).unwrap()
}

#[test]
fn settings_says_the_server_is_managed_only_when_the_installed_unit_started_it() {
    let world = World::new("settings");
    let (_by_hand, addr) = world.serve_by_hand();
    let server = &settings(&addr)["server"];
    assert_eq!(server["managed_by_systemd"], false, "{server}");
    assert_eq!(server["supervision"], "Not managed: stops on reboot", "{server}");
    // A shell under some other unit (a terminal's, say) passes its systemd variables on to what it starts;
    // nothing restarts that server, so it is not managed.
    let inherited: [&[(&str, &str)]; 3] = [&[("INVOCATION_ID", "0123abcd")], &[("NOTIFY_SOCKET", "/run/user/1000/systemd/notify")], &[("ORQ_SYSTEMD_UNIT", "orq-serve.service")]];
    for env in inherited {
        let (_inherited, addr) = world.serve_with(env);
        let server = &settings(&addr)["server"];
        assert_eq!(server["managed_by_systemd"], false, "{env:?}: {server}");
        assert_eq!(server["supervision"], "Not managed: stops on reboot", "{env:?}: {server}");
    }
    // Only the unit `orq serve install` writes, which restarts it, makes it managed.
    let (_managed, addr) = world.serve_with(&[("INVOCATION_ID", "0123abcd"), ("ORQ_SYSTEMD_UNIT", "orq-serve.service")]);
    let server = &settings(&addr)["server"];
    assert_eq!(server["managed_by_systemd"], true, "{server}");
    assert_eq!(server["supervision"], "Managed by systemd (restarts automatically)", "{server}");
}

#[test]
fn settings_warns_of_a_missing_harness_only_when_the_unit_runs_the_server() {
    let world = World::new("settingswarn");
    let path = world.shell_path(&[]);
    let (_by_hand, addr) = world.serve_with(&[("PATH", &path)]);
    let server = &settings(&addr)["server"];
    assert_eq!(server["harness_warnings"], serde_json::json!([]), "a server started by hand has its shell's PATH: {server}");
    let (_managed, addr) = world.serve_with(&[("PATH", &path), ("INVOCATION_ID", "0123abcd"), ("ORQ_SYSTEMD_UNIT", "orq-serve.service")]);
    let server = &settings(&addr)["server"];
    let warnings = server["harness_warnings"].as_array().unwrap_or_else(|| panic!("{server}"));
    let claude = format!("runs will fail: claude is not on the PATH the service gets ({path})");
    assert!(warnings.iter().any(|warning| warning == &serde_json::json!(claude)), "{server}");
}

#[test]
fn settings_stops_warning_once_the_missing_harness_is_put_on_the_servers_path() {
    let world = World::new("settingsfresh");
    let tools = world.root.join("tools");
    fs::create_dir_all(&tools).unwrap();
    let path = world.shell_path(&[tools.to_str().unwrap()]);
    let (_managed, addr) = world.serve_with(&[("PATH", &path), ("INVOCATION_ID", "0123abcd"), ("ORQ_SYSTEMD_UNIT", "orq-serve.service")]);
    let claude = format!("runs will fail: claude is not on the PATH the service gets ({path})");
    let server = &settings(&addr)["server"];
    assert!(server["harness_warnings"].as_array().unwrap().contains(&serde_json::json!(claude)), "{server}");
    fs::write(tools.join("claude"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(tools.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    let server = &settings(&addr)["server"];
    assert!(!server["harness_warnings"].as_array().unwrap().contains(&serde_json::json!(claude)), "claude is there now: {server}");
}

#[test]
fn quotes_percent_and_dollar_signs_in_paths_are_escaped_in_the_unit_and_read_back_by_status() {
    let mut world = World::new("escapes");
    let odd = world.root.join("odd \"bin\" 100% $HOME \\ dir");
    fs::create_dir_all(&odd).unwrap();
    let installed = odd.join("orq");
    fs::copy(&world.installed, &installed).unwrap();
    world.installed = installed;
    ok(&world.install("127.0.0.1:7999"));
    let unit = world.unit();
    let escaped = format!("ExecStart=\"{}/odd \\\"bin\\\" 100%% $$HOME \\\\ dir/orq\" \"serve\"", world.root.display());
    assert!(unit.contains(&escaped), "{escaped}\n{unit}");
    let out = ok(&world.serve(&["status"]));
    assert!(out.contains(&format!("binary: {}\n", world.installed.display())), "{out}");
}

#[test]
fn the_units_own_server_on_the_address_does_not_hold_up_a_reinstall() {
    let world = World::new("own");
    let (_unit_server, addr) = world.serve_with(&[("INVOCATION_ID", "0123abcd"), ("ORQ_SYSTEMD_UNIT", "orq-serve.service")]);
    ok(&world.install(&addr));
    // Another unit's server is not this unit's.
    let other = World::new("other-unit");
    let (server, addr) = other.serve_with(&[("INVOCATION_ID", "0123abcd"), ("ORQ_SYSTEMD_UNIT", "something-else.service")]);
    let out = other.install(&addr);
    assert!(!out.status.success() && text(&out.stderr).contains(&format!("pid {}", server.0.id())), "{}", text(&out.stderr));
}

#[test]
fn an_identical_install_after_a_failed_one_enables_and_starts_the_unit() {
    let world = World::new("recover");
    world.answer("systemctl", "case \"$*\" in *enable*) echo 'Failed to enable unit' >&2; exit 1;; esac");
    let failed = world.install("127.0.0.1:7999");
    assert!(!failed.status.success(), "{}", text(&failed.stdout));
    assert!(text(&failed.stderr).contains("Failed to enable unit"), "{}", text(&failed.stderr));
    // The unit is written but disabled and stopped: installing again finishes the job.
    world.answer("systemctl", "case \"$*\" in *show*) printf 'UnitFileState=disabled\\nActiveState=inactive\\nNRestarts=0\\n';; esac");
    world.forget_calls();
    let out = ok(&world.install("127.0.0.1:7999"));
    assert!(!out.contains("already installed"), "{out}");
    assert_eq!(world.systemctl_calls(), format!("{SHOW}systemctl --user daemon-reload\nsystemctl --user enable --now orq-serve.service\n"));
}

#[test]
fn install_refuses_a_bad_passkey_host_or_a_folder_that_is_not_a_git_project_before_writing_anything() {
    let world = World::new("validate");
    let repo = world.repo.display().to_string();
    let bad_host = world.serve(&["install", "--repo", &repo, "--listen", "127.0.0.1:7999", "--passkey-host", "Not A Host"]);
    assert!(!bad_host.status.success(), "{}", text(&bad_host.stdout));
    assert!(text(&bad_host.stderr).contains("the passkey host"), "{}", text(&bad_host.stderr));
    let plain = world.root.join("plain folder");
    fs::create_dir_all(&plain).unwrap();
    let plain = plain.display().to_string();
    let not_git = world.serve(&["install", "--repo", &plain, "--listen", "127.0.0.1:7999"]);
    assert!(!not_git.status.success(), "{}", text(&not_git.stdout));
    assert!(text(&not_git.stderr).contains("cannot resolve project"), "{}", text(&not_git.stderr));
    assert!(!world.unit_path().exists(), "nothing is written");
    assert_eq!(world.systemctl_calls(), "", "systemd is never asked");
}

#[test]
fn uninstall_keeps_the_unit_file_when_systemd_cannot_stop_it() {
    let world = World::new("uninstall-fails");
    ok(&world.install("127.0.0.1:7999"));
    let unit = world.unit();
    world.answer("systemctl", "case \"$*\" in *disable*) echo 'Access denied' >&2; exit 1;; esac");
    let out = world.serve(&["uninstall"]);
    assert!(!out.status.success(), "{}", text(&out.stdout));
    assert!(text(&out.stderr).contains("Access denied"), "{}", text(&out.stderr));
    assert_eq!(world.unit(), unit, "the file stays for another try");
}

#[test]
fn status_shows_what_systemd_and_the_journal_cannot_tell_as_unknown_and_why() {
    let world = World::new("status-fails");
    ok(&world.install("127.0.0.1:7999"));
    world.answer("systemctl", "echo 'Failed to connect to bus' >&2; exit 1");
    world.answer("journalctl", "echo 'No journal files were found' >&2; exit 1");
    world.answer("loginctl", "exit 1");
    let out = ok(&world.serve(&["status"]));
    assert!(out.contains("enabled, active, restarts: unknown ("), "{out}");
    assert!(out.contains("Failed to connect to bus"), "{out}");
    assert!(out.contains("last journal lines: unknown ("), "{out}");
    assert!(out.contains("No journal files were found"), "{out}");
    assert!(out.contains("linger: no"), "unknown linger counts as off: {out}");
    assert!(out.contains(&format!("binary: {}", world.installed.display())), "{out}");
}

#[test]
fn an_update_that_fails_at_any_step_keeps_the_old_unit_so_installing_again_applies_it() {
    for step in ["daemon-reload", "enable", "restart"] {
        let world = World::new(&format!("update-fails-{step}"));
        ok(&world.install("127.0.0.1:7999"));
        let old = world.unit();
        // The old server stays enabled and running through it all.
        world.answer("systemctl", &format!("case \"$*\" in *show*) printf 'UnitFileState=enabled\\nActiveState=active\\nNRestarts=0\\n';; *{step}*) echo 'Job failed' >&2; exit 1;; esac"));
        let failed = world.install("127.0.0.1:7998");
        assert!(!failed.status.success(), "{step}: {}", text(&failed.stdout));
        assert!(text(&failed.stderr).contains("Job failed"), "{step}: {}", text(&failed.stderr));
        assert_eq!(world.unit(), old, "{step}: the unit the running server came from is put back");
        world.healthy();
        world.forget_calls();
        let out = ok(&world.install("127.0.0.1:7998"));
        assert!(out.contains("updated"), "{step}: {out}");
        assert!(world.unit().contains("127.0.0.1:7998"), "{step}");
        assert_eq!(world.systemctl_calls(), "systemctl --user daemon-reload\nsystemctl --user enable orq-serve.service\nsystemctl --user restart orq-serve.service\n", "{step}");
    }
}
