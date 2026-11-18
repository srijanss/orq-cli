//! `orq serve`: starts Orq's web server. `orq remote serve`, its older form, was removed and only
//! points here.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::{fail, usage_error};

/// Where a plain `orq serve` listens unless told otherwise: this machine only.
const DEFAULT_LISTEN: &str = "127.0.0.1:7878";

/// `orq serve [--repo <path>] [--listen <addr:port>] [--passkey-host <name> [--no-loopback-token]] [--upgrade-now]`: the same server with nothing to set up. The
/// command itself is the opt-in, so the run settings need not enable remote control, a missing
/// token is created, and the repository defaults to the current directory.
pub fn serve_plain(args: &[OsString]) -> ExitCode {
    match args.first().and_then(|word| word.to_str()) {
        Some("restart") => restart_command(&args[1..]),
        _ => run(args),
    }
}

/// `orq serve restart (--when-idle | --now [--force])`: asks each running `orq serve` of this user to
/// restart into the installed orq, once no run is active or now. `--now` refuses over active runs,
/// naming them, unless `--force`; a server with no binary to restart into is refused with the reason.
fn restart_command(args: &[OsString]) -> ExitCode {
    let words: Vec<Option<&str>> = args.iter().map(|word| word.to_str()).collect();
    let (now, force) = match words.as_slice() {
        [Some("--when-idle")] => (false, false),
        [Some("--now")] => (true, false),
        [Some("--now"), Some("--force")] | [Some("--force"), Some("--now")] => (true, true),
        _ => return usage_error(),
    };
    let roots = match orq_core::storage::StorageRoots::resolve(&|name| std::env::var(name).ok()) {
        Ok(roots) => roots,
        Err(err) => return fail(format!("cannot locate the Orq directories: {err}")),
    };
    let servers = live_servers(&roots);
    if servers.is_empty() {
        return fail("no orq serve is running");
    }
    let active = match active_run_ids(&roots.state) {
        Ok(active) => active,
        Err(why) if now && !force => return fail(format!("cannot tell which runs are active, so not restarting now: {why}")),
        Err(why) => vec![why],
    };
    if now && !force && !active.is_empty() {
        return fail(format!("not restarting now: {} active ({}); use --when-idle to wait for them, or --force", runs(active.len()), active.join(", ")));
    }
    let installed = installed_binary();
    for server in &servers {
        if let Err(why) = restart_target(&server.exe, &installed) {
            return fail(format!("cannot restart orq serve (pid {}): {why}", server.pid));
        }
    }
    let signal = restart_signal(now, force);
    for server in &servers {
        // SAFETY: kill only sends a signal; the pid is a live server's, held by its lock.
        if unsafe { libc::kill(server.pid as libc::pid_t, signal) } != 0 {
            return fail(format!("cannot signal orq serve (pid {}): {}", server.pid, std::io::Error::last_os_error()));
        }
        match (now, active.len()) {
            (true, _) | (false, 0) => println!("orq serve (pid {}, {}): restarting now", server.pid, server.listen),
            (false, waiting) => println!("orq serve (pid {}, {}): restart pending, waiting for {}", server.pid, server.listen, runs(waiting)),
        }
    }
    ExitCode::SUCCESS
}

/// The signal that asks a server to restart. Only a forced restart goes ahead at once (SIGUSR2); any other
/// asks it to restart when idle (SIGUSR1), so the server looks at the runs itself when it takes the signal
/// and waits for one that started after `--now` found none.
fn restart_signal(now: bool, force: bool) -> libc::c_int {
    if now && force { libc::SIGUSR2 } else { libc::SIGUSR1 }
}

/// `1 run`, `2 runs`.
fn runs(count: usize) -> String {
    format!("{count} run{}", if count == 1 { "" } else { "s" })
}

/// A running `orq serve`, as its server file names it.
struct LiveServer {
    pid: u32,
    exe: PathBuf,
    listen: String,
}

/// Where each running server keeps a file naming it, locked for as long as it runs.
fn serve_dir(roots: &orq_core::storage::StorageRoots) -> PathBuf {
    roots.state.join("serve")
}

/// The servers whose files are still locked; a file no server holds is left over and removed.
fn live_servers(roots: &orq_core::storage::StorageRoots) -> Vec<LiveServer> {
    use std::os::fd::AsRawFd;
    let Ok(entries) = std::fs::read_dir(serve_dir(roots)) else { return Vec::new() };
    let mut live = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // Only a real process id: 0 would signal the process group, and one past pid_t a group or every process.
        let Some(own_pid) = path.file_stem().and_then(|stem| stem.to_str()).and_then(|stem| stem.parse::<libc::pid_t>().ok()).filter(|pid| *pid > 0) else { continue };
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Ok(file) = std::fs::File::open(&path) else { continue };
        // SAFETY: flock on a descriptor this function owns.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        // Held by its server only when the lock would block; any other failure says nothing about it.
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::EWOULDBLOCK) {
            continue;
        }
        let Ok(info) = std::fs::read_to_string(&path).map_err(|_| ()).and_then(|text| serde_json::from_str::<serde_json::Value>(&text).map_err(|_| ())) else { continue };
        let (Some(pid), Some(exe)) = (info["pid"].as_u64(), info["exe"].as_str()) else { continue };
        // The file a server writes is named by its own pid; one that says otherwise is not that server's.
        if pid != own_pid as u64 {
            continue;
        }
        live.push(LiveServer { pid: own_pid as u32, exe: PathBuf::from(exe), listen: info["listen"].as_str().unwrap_or_default().to_string() });
    }
    live
}

/// Writes this server's file and locks it; the lock goes with the returned file, when it is dropped or
/// the process execs or ends. `None` when it could not be written, so `orq serve restart` will not see it.
fn announce_server(roots: &orq_core::storage::StorageRoots, listen: &str) -> Option<(PathBuf, std::fs::File)> {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    let dir = serve_dir(roots);
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}.json", std::process::id()));
    let mut file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path).ok()?;
    // SAFETY: flock on a descriptor this function owns.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let info = serde_json::json!({ "pid": std::process::id(), "exe": exe, "listen": listen, "version": format!("orq {BUILD}") });
    file.set_len(0).ok()?;
    file.write_all(info.to_string().as_bytes()).ok()?;
    Some((path, file))
}

/// The file a restarting server leaves for the one after it, saying which build it was; per address, so
/// the server started in its place on the same address finds it.
fn restart_marker(roots: &orq_core::storage::StorageRoots, listen: &str) -> PathBuf {
    let key: String = listen.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    serve_dir(roots).join(format!("restarted-{key}.json"))
}

/// When this server was started by a restart, audits it with both builds and says so.
fn note_restarted(roots: &orq_core::storage::StorageRoots, listen: &str) {
    let marker = restart_marker(roots, listen);
    let Ok(text) = std::fs::read_to_string(&marker) else { return };
    let _ = std::fs::remove_file(&marker);
    let from = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|info| info["from"].as_str().map(str::to_string)).unwrap_or_else(|| "an unknown build".into());
    let reason = format!("from {from} to orq {BUILD}");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let attempt = orq_core::remote_audit::Attempt {
        identity: "orq serve",
        action: "server.restarted",
        project: "server",
        run: None,
        outcome: orq_core::remote_audit::Outcome::Allowed,
        reason: Some(&reason),
    };
    if let Err(why) = orq_core::remote_audit::record(roots, &attempt, now) {
        eprintln!("orq serve: cannot audit the restart: {why}");
    }
    println!("orq serve: restarted {reason}");
}

/// This build: its version, git commit and build time, as `orq --version` prints it after `orq `.
pub(crate) const BUILD: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("ORQ_BUILD_COMMIT"), " ", env!("ORQ_BUILD_TIME"), ")");

/// What `orq remote serve`, which was removed, says instead of starting anything.
pub const REMOTE_SERVE_REMOVED: &str = "use `orq serve` (same flags)";

fn run(args: &[OsString]) -> ExitCode {
    let mut repo = None;
    let mut listen = String::from(DEFAULT_LISTEN);
    let mut passkey_host = None;
    let mut loopback_token = true;
    let mut upgrade_now = false;
    let mut words = args.iter();
    while let Some(word) = words.next() {
        match word.to_str() {
            Some("--repo") => match words.next() {
                Some(path) if repo.is_none() => repo = Some(PathBuf::from(path)),
                _ => return usage_error(),
            },
            Some("--listen") => match words.next().and_then(|a| a.to_str()) {
                Some(address) => listen = address.to_string(),
                None => return usage_error(),
            },
            Some("--passkey-host") => match words.next().and_then(|a| a.to_str()) {
                Some(host) if passkey_host.is_none() => passkey_host = Some(host.to_string()),
                _ => return usage_error(),
            },
            Some("--no-loopback-token") if loopback_token => loopback_token = false,
            Some("--upgrade-now") if !upgrade_now => upgrade_now = true,
            _ => return usage_error(),
        }
    }
    if !loopback_token && passkey_host.is_none() {
        return fail("--no-loopback-token only applies together with --passkey-host");
    }
    let auth = match passkey_host.as_deref().map(orq_core::passkey_register::PasskeyHost::from_setting) {
        Some(Ok(host)) => Some(std::sync::Arc::new(orq_core::passkey_routes::PasskeyAuth::new(host).with_loopback_token(loopback_token))),
        Some(Err(reason)) => return fail(reason),
        None => None,
    };
    let repo = match repo {
        Some(repo) => repo,
        None => match std::env::current_dir() {
            Ok(dir) => dir,
            Err(err) => return fail(format!("cannot read the current directory: {:?}", err.kind())),
        },
    };
    // The run settings are still read, so a broken one is reported here as before; `remote_control`
    // is Claude Remote Control for sessions and has no say over serving the web.
    if let Err(err) = orq_orchestrator::load_run_settings(&repo, &|name| std::env::var(name).ok()) {
        return fail(format!("cannot read the run settings: {err}"));
    }
    let roots = match orq_core::storage::StorageRoots::resolve(&|name| std::env::var(name).ok()) {
        Ok(roots) => roots,
        Err(err) => return fail(format!("cannot locate the Orq directories: {err}")),
    };
    // Before anything opens orq.db, which upgrades it.
    if let Err(reason) = orq_remote::projects::check_upgrade(&roots, upgrade_now) {
        return fail(reason);
    }
    let config = match orq_core::remote_inbox::RemoteConfig::load_or_create(&roots) {
        Ok(config) => config,
        Err(reason) => return fail(reason),
    };
    let project = match orq_git::resolve_project(&repo) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    install_interrupt_handlers();
    let listener = match std::net::TcpListener::bind(&listen) {
        Ok(listener) => listener,
        Err(err) => return fail(format!("cannot listen on {listen}: {:?}", err.kind())),
    };
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_absolute());
    let override_root = std::env::var_os("ORQ_REMOTE_PROJECTS_ROOT").map(PathBuf::from).filter(|root| root.is_absolute());
    let scan_root = orq_remote::projects::default_root(home.as_deref(), override_root.as_deref());
    let launcher = std::env::current_exe().ok().map(|exe| {
        let user_config = orq_features::settings_path(std::env::var_os("XDG_CONFIG_HOME").as_deref(), std::env::var_os("HOME").as_deref());
        std::sync::Arc::new(crate::remote_launch::CliLauncher::new(exe, user_config))
    });
    let restart = std::sync::Arc::new(restart_control(&roots));
    let listen_key = listen.clone();
    let mut projects = orq_remote::projects::StoreProjects::new(roots.clone(), scan_root).with_server(restart.clone());
    if let Some(launcher) = launcher {
        projects = projects.with_launcher(launcher);
    }
    // The web chooses agents from the same list as the popup.
    projects = projects.with_agent_options(std::sync::Arc::new(|| crate::agent_options(&crate::find_program)));
    // The web cleans up storage as the popup does: the same preview, the same prune and its lock.
    projects = projects.with_storage(std::sync::Arc::new(crate::storage::WebCleanup::new()));
    // Reports runs could not build are built again from their events, as the runs would have.
    projects = projects.with_report_builder(std::sync::Arc::new(crate::pipeline::repair_review_report));
    let projects = std::sync::Arc::new(projects);
    let state_roots = roots.clone();
    let state = std::sync::Arc::new(orq_remote::ServerState {
        roots,
        config,
        project_id: crate::pipeline::project_id(&project),
        project_name: project.name.clone(),
    });
    let reviews = orq_remote::review_refresh::ReviewRefresher::new(state_roots.clone(), std::sync::Arc::new(ProviderLook { roots: state_roots.clone() }));
    let queued = projects.clone();
    let options = orq_remote::ServerOptions { projects: Some(projects), auth: auth.clone(), reviews: Some(reviews.clone()) };
    let started = orq_remote::Server::start_with(listener, state, options);
    let server = match started {
        Ok(server) => server,
        Err(err) => return fail(format!("cannot start the server: {:?}", err.kind())),
    };
    // Audited before the address is announced, so whoever waits for it finds the line.
    note_restarted(&state_roots, &listen_key);
    println!("orq remote: listening on http://{}", server.addr());
    let announced = announce_server(&state_roots, &server.addr().to_string());
    if let Some(auth) = &auth {
        println!("passkeys: sign in at {} (serve it over HTTPS, for example `tailscale serve --bg {}`)", auth.host().origin(), server.addr().port());
    }
    let token = orq_core::remote_inbox::RemoteConfig::token_path(&state_roots);
    println!("sign in with the token stored in {}", token.display());
    println!("read it with: cat -- {}", shell_quote(&token.display().to_string()));
    let _ = std::io::Write::flush(&mut std::io::stdout());
    // Open provider reviews are looked at again while the server runs.
    let rounds = orq_remote::review_refresh::start_rounds(reviews, orq_remote::review_refresh::ROUND_EVERY);
    // What runs could not record is repaired now and on each review round.
    let repair_rounds = orq_remote::projects::start_repair_rounds(queued.clone(), orq_remote::review_refresh::ROUND_EVERY);
    // Queued features start on their own, but not while a restart waits for the runs to end.
    let hold = restart.clone();
    let queue_rounds = orq_remote::projects::start_queue_rounds(queued, std::time::Duration::from_secs(5), move || orq_core::remote_inbox::ServerControl::restart_pending(&*hold));
    let stop = wait_for_stop(&restart);
    // In-flight requests finish and event streams close as the server stops.
    drop(queue_rounds);
    drop(repair_rounds);
    drop(server);
    drop(rounds);
    if let Some((path, file)) = announced {
        let _ = std::fs::remove_file(path);
        drop(file);
    }
    let Stop::Restart(target) = stop else { return ExitCode::SUCCESS };
    let marker = restart_marker(&state_roots, &listen_key);
    if let Err(err) = std::fs::write(&marker, serde_json::json!({ "from": format!("orq {BUILD}") }).to_string()) {
        eprintln!("orq serve: cannot note the restart for the next server: {:?}", err.kind());
    }
    println!("orq serve: restarting into {}", target.display());
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut command = std::process::Command::new(&target);
    command.args(std::env::args_os().skip(1));
    let err = std::os::unix::process::CommandExt::exec(&mut command);
    let _ = std::fs::remove_file(&marker);
    fail(format!("cannot start {}: {err}", target.display()))
}

/// Why the server stopped: interrupted, or restarting into a binary.
enum Stop {
    Interrupted,
    Restart(PathBuf),
}

/// Blocks until a signal stops the server or a restart is due. A restart asked for by signal (SIGUSR1
/// when idle, SIGUSR2 now) or from the web goes ahead once [`RestartControl::due`]; the active runs are
/// looked at about once a second while one waits.
fn wait_for_stop(restart: &RestartControl) -> Stop {
    use std::sync::atomic::Ordering::SeqCst;
    let mut last_look: Option<std::time::Instant> = None;
    loop {
        if INTERRUPTED.load(SeqCst) {
            return Stop::Interrupted;
        }
        let mut asked = false;
        if RESTART_WHEN_IDLE.swap(false, SeqCst) {
            asked = true;
            match orq_core::remote_inbox::ServerControl::restart_when_idle(restart) {
                Ok(waiting) => println!("orq serve: restart pending, waiting for {}", runs(waiting)),
                Err(why) => eprintln!("orq serve: cannot restart: {why}"),
            }
        }
        if RESTART_NOW.swap(false, SeqCst) {
            asked = true;
            restart.restart_now();
        }
        let looked_lately = last_look.is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(1));
        if asked || !looked_lately {
            last_look = Some(std::time::Instant::now());
            if restart.due() {
                match restart.target() {
                    Ok(target) => return Stop::Restart(target),
                    Err(why) => {
                        eprintln!("orq serve: cannot restart: {why}");
                        orq_core::remote_inbox::ServerControl::cancel_restart(restart);
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Looks at a run's review at its provider with `review::refresh_lifecycle`. A look that cannot be made is
/// kept on the review as its last error, so it shows unverified and waits for the next round.
struct ProviderLook {
    roots: orq_core::storage::StorageRoots,
}

impl orq_remote::review_refresh::ReviewObserver for ProviderLook {
    fn refresh(&self, run_id: &str) {
        let Err(err) = crate::review::refresh_lifecycle(&self.roots, run_id) else { return };
        let Ok(store) = orq_core::store::Store::open(&self.roots.database()) else { return };
        let latest = store.lifecycles_for_run(run_id).ok().and_then(|all| all.into_iter().filter(|l| l.provider != orq_core::review_lifecycle::LOCAL_PROVIDER).last());
        if let Some(latest) = latest {
            let failed = latest.apply(Err(orq_core::review_lifecycle::ObservationError::Unavailable(err)));
            let _ = store.save_lifecycle(run_id, &failed);
        }
    }
}

/// Single-quotes `text` for a POSIX shell, so a path with spaces or metacharacters pastes safely.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RESTART_WHEN_IDLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RESTART_NOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn note_restart_when_idle(_: libc::c_int) {
    RESTART_WHEN_IDLE.store(true, std::sync::atomic::Ordering::SeqCst);
}

extern "C" fn note_restart_now(_: libc::c_int) {
    RESTART_NOW.store(true, std::sync::atomic::Ordering::SeqCst);
}

extern "C" fn note_interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Makes Ctrl-C and a terminate signal set the flag instead of killing the
/// process; installed before the address is announced so no signal is missed.
fn install_interrupt_handlers() {
    // SAFETY: the handler only stores to an atomic flag.
    unsafe {
        libc::signal(libc::SIGINT, note_interrupt as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, note_interrupt as *const () as libc::sighandler_t);
        // `orq serve restart` asks by signal: SIGUSR1 to restart when idle, SIGUSR2 now.
        libc::signal(libc::SIGUSR1, note_restart_when_idle as *const () as libc::sighandler_t);
        libc::signal(libc::SIGUSR2, note_restart_now as *const () as libc::sighandler_t);
    }
}


/// Whether `path` lies under a `target` directory, where cargo puts its builds.
fn under_target(path: &std::path::Path) -> bool {
    path.components().any(|part| part.as_os_str() == "target")
}

/// The binary a restart starts: the running one, unless it is a build under `target/`, which is never
/// started again; then `installed`, if it is a file and not a build either. Otherwise why there is none.
fn restart_target(exe: &std::path::Path, installed: &std::path::Path) -> Result<PathBuf, String> {
    let exe = on_disk(exe);
    if !under_target(&exe) {
        runnable(&exe)?;
        return Ok(exe);
    }
    if installed.exists() && !under_target(installed) {
        runnable(installed)?;
        return Ok(installed.to_path_buf());
    }
    Err(format!("orq runs from a build under target/ ({}) and no installed orq is at {}", exe.display(), installed.display()))
}

/// Linux names a binary replaced on disk "<path> (deleted)"; the path is what is started again.
fn on_disk(exe: &std::path::Path) -> PathBuf {
    match exe.to_str().and_then(|text| text.strip_suffix(" (deleted)")) {
        Some(stripped) => PathBuf::from(stripped),
        None => exe.to_path_buf(),
    }
}

/// Whether `path` can be started: an executable regular file. Checked before the server stops, so a
/// binary that is gone or cannot run refuses the restart instead of leaving no server.
fn runnable(path: &std::path::Path) -> Result<(), String> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() && executable_by_this_process(path) => Ok(()),
        Ok(meta) if meta.is_file() => Err(format!("{} is not executable by this user", path.display())),
        Ok(_) => Err(format!("{} is not a file", path.display())),
        Err(err) => Err(format!("cannot start {}: {:?}", path.display(), err.kind())),
    }
}

/// Whether this process may execute `path`, by its effective user and groups, as exec will decide: the
/// owner's bits apply to the owner even when the others' allow it.
fn executable_by_this_process(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: a valid NUL-terminated path; faccessat only reads it.
    unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::X_OK, libc::AT_EACCESS) == 0 }
}

/// The runs a restart waits for: the active runs as the Runs view counts them, less the paused ones,
/// which wait at a stage boundary and resume under any server. A run that cannot be read is an error,
/// so a restart never goes ahead over a run it could not look at.
fn active_run_ids(state: &std::path::Path) -> Result<Vec<String>, String> {
    use orq_orchestrator::{RunPhase, StoreError};
    let store = orq_orchestrator::RunStore::new(state);
    let mut all = Vec::new();
    for run_id in store.run_ids().map_err(|error| format!("cannot list the runs: {error:?}"))? {
        let record = match store.load(&run_id) {
            Ok(record) => record,
            Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => continue,
            Err(error) => return Err(format!("run {run_id} is unreadable: {error:?}")),
        };
        let events = store.events(&run_id).map_err(|error| format!("run {run_id} is unreadable: {error:?}"))?;
        all.push((run_id, record, events));
    }
    let paused: std::collections::HashSet<&str> =
        all.iter().filter(|(_, _, events)| orq_orchestrator::summarize_run(events).phase == RunPhase::Paused).map(|(run_id, _, _)| run_id.as_str()).collect();
    Ok(orq_orchestrator::active_runs(&all).into_iter().map(|(run_id, _)| run_id).filter(|run_id| !paused.contains(run_id.as_str())).collect())
}

/// Where the installed orq is: `ORQ_INSTALLED_BINARY` when set (tests name a stand-in), else /usr/local/bin/orq.
fn installed_binary() -> PathBuf {
    std::env::var_os("ORQ_INSTALLED_BINARY").filter(|path| !path.is_empty()).map_or_else(|| PathBuf::from("/usr/local/bin/orq"), PathBuf::from)
}

/// This server's controls: it waits for the runs under `roots`, and restarts into [`restart_target`].
fn restart_control(roots: &orq_core::storage::StorageRoots) -> RestartControl {
    let state = roots.state.clone();
    let installed = installed_binary();
    let target_installed = installed.clone();
    RestartControl::new(
        format!("orq {BUILD}"),
        installed,
        // A store that cannot be read holds the restart back: it may hide a run.
        Box::new(move || active_run_ids(&state).unwrap_or_else(|why| vec![why])),
        Box::new(move || {
            let exe = std::env::current_exe().map_err(|err| format!("cannot find the running orq: {:?}", err.kind()))?;
            restart_target(&exe, &target_installed)
        }),
    )
}

/// The runs a restart waits for, by id.
type ActiveRuns = Box<dyn Fn() -> Vec<String> + Send + Sync>;
/// The binary a restart would start, or why there is none.
type RestartTarget = Box<dyn Fn() -> Result<PathBuf, String> + Send + Sync>;

/// The server's own controls: its build and the installed one's, and a restart that waits until no
/// run is active (or one asked for now, which waits for nothing).
pub(crate) struct RestartControl {
    running: String,
    installed: PathBuf,
    /// The installed binary's `--version`, kept until its modification time changes.
    installed_version: std::sync::Mutex<Option<(std::time::SystemTime, Option<String>)>>,
    active: ActiveRuns,
    target: RestartTarget,
    pending: std::sync::atomic::AtomicBool,
    now: std::sync::atomic::AtomicBool,
}

impl RestartControl {
    pub(crate) fn new(running: String, installed: PathBuf, active: ActiveRuns, target: RestartTarget) -> Self {
        RestartControl {
            running,
            installed,
            installed_version: Default::default(),
            active,
            target,
            pending: Default::default(),
            now: Default::default(),
        }
    }

    /// The installed orq's version, read again only when the file changed; `None` when there is none.
    fn installed_version(&self) -> Option<String> {
        let modified = std::fs::metadata(&self.installed).and_then(|meta| meta.modified()).ok()?;
        let mut cached = self.installed_version.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((at, version)) = cached.as_ref() {
            if *at == modified {
                return version.clone();
            }
        }
        let version = crate::herdr_version(&self.installed.clone().into_os_string()).map(|out| out.trim().to_string()).filter(|out| !out.is_empty());
        *cached = Some((modified, version.clone()));
        version
    }

    /// Asks for a restart that waits for nothing.
    pub(crate) fn restart_now(&self) {
        self.now.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether the server should restart now: asked to now, or asked to when idle and no run is active.
    pub(crate) fn due(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        self.now.load(SeqCst) || (self.pending.load(SeqCst) && (self.active)().is_empty())
    }

    /// The binary the restart starts.
    pub(crate) fn target(&self) -> Result<PathBuf, String> {
        (self.target)()
    }
}

impl orq_core::remote_inbox::ServerControl for RestartControl {
    fn status(&self) -> serde_json::Value {
        let installed = self.installed_version();
        let pending = self.restart_pending();
        let waiting_for = if pending { (self.active)().len() } else { 0 };
        serde_json::json!({
            "running": self.running,
            "newer_installed": installed.as_ref().is_some_and(|installed| *installed != self.running),
            "installed": installed,
            "restart_pending": pending,
            "waiting_for": waiting_for,
        })
    }

    fn restart_when_idle(&self) -> Result<usize, String> {
        self.target()?;
        self.pending.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok((self.active)().len())
    }

    fn cancel_restart(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        // Both go: a restart asked for now that cannot go ahead is dropped the same way.
        self.now.swap(false, SeqCst) | self.pending.swap(false, SeqCst)
    }

    fn restart_pending(&self) -> bool {
        self.pending.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
mod restart_tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("orq-restart-unit-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn live_servers_only_names_a_pid_that_is_the_files_own_and_a_real_process_id() {
        use std::os::fd::AsRawFd;
        let base = temp("live");
        let roots = orq_core::storage::StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
        let dir = serve_dir(&roots);
        std::fs::create_dir_all(&dir).unwrap();
        // Each file held as its server holds it, so all of them look live.
        let held: Vec<std::fs::File> = [("0", 0u64), ("4294967295", 4_294_967_295), ("7", 8), ("9", 9)]
            .into_iter()
            .map(|(stem, pid)| {
                let path = dir.join(format!("{stem}.json"));
                std::fs::write(&path, serde_json::json!({ "pid": pid, "exe": "/usr/local/bin/orq", "listen": "127.0.0.1:1" }).to_string()).unwrap();
                let file = std::fs::File::open(&path).unwrap();
                // SAFETY: flock on a descriptor the test owns.
                assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
                file
            })
            .collect();
        let pids: Vec<u32> = live_servers(&roots).iter().map(|server| server.pid).collect();
        assert_eq!(pids, vec![9], "pid 0 signals a process group, a pid past pid_t wraps to one, and a pid that is not the file's own is not this server");
        drop(held);
    }

    #[test]
    fn only_a_forced_restart_skips_the_servers_own_idle_check() {
        // An unforced --now found no run active, but one may start before the server takes the signal: the
        // server looks again (as when idle), so the restart never goes ahead over it.
        assert_eq!(restart_signal(false, false), libc::SIGUSR1);
        assert_eq!(restart_signal(true, false), libc::SIGUSR1);
        assert_eq!(restart_signal(true, true), libc::SIGUSR2);
    }

    #[test]
    fn runnable_needs_execute_permission_for_this_user_not_just_any_execute_bit() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return; // root may run any file with an execute bit
        }
        let dir = temp("others-only");
        let path = dir.join("orq");
        executable(&path);
        // The owner's bits apply to the owner, so `r--` wins over the others' `x`.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o645)).unwrap();
        let refused = runnable(&path).unwrap_err();
        assert!(refused.contains("not executable"), "{refused}");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o744)).unwrap();
        assert_eq!(runnable(&path), Ok(()));
    }

    fn executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn restart_target_is_refused_unless_it_is_an_executable_file_so_the_server_never_stops_for_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp("unrunnable");
        let built = dir.join("repo/target/release/orq");
        let plain = dir.join("bin/orq");
        std::fs::create_dir_all(plain.parent().unwrap()).unwrap();
        std::fs::write(&plain, "").unwrap();
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
        let refused = restart_target(&built, &plain).unwrap_err();
        assert!(refused.contains("not executable") && refused.contains(&plain.display().to_string()), "{refused}");
        let removed = dir.join("usr/local/bin/orq");
        let refused = restart_target(&removed, &plain).unwrap_err();
        assert!(refused.contains(&removed.display().to_string()), "an installed binary that is gone is refused: {refused}");
        let folder = dir.join("folder/orq");
        std::fs::create_dir_all(&folder).unwrap();
        assert!(restart_target(&folder, &plain).is_err(), "a directory is not a binary");
        assert!(restart_target(&plain, &plain).is_err(), "nor is a file without the execute bit");
    }

    #[test]
    fn restart_target_keeps_an_installed_binary_and_never_one_under_target() {
        let dir = temp("target");
        let installed = dir.join("bin/orq");
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        executable(&installed);
        let usr = dir.join("usr/local/bin/orq");
        executable(&usr);
        assert_eq!(restart_target(&usr, &installed), Ok(usr.clone()), "an installed binary re-execs itself");
        let built = dir.join("repo/target/release/orq");
        assert_eq!(restart_target(&built, &installed), Ok(installed.clone()), "a build under target/ restarts into the installed one");
        let deleted = PathBuf::from(format!("{} (deleted)", built.display()));
        assert_eq!(restart_target(&deleted, &installed), Ok(installed.clone()));
        let missing = dir.join("nothing/orq");
        let refused = restart_target(&built, &missing).unwrap_err();
        assert!(refused.contains("target/") && refused.contains(&missing.display().to_string()), "{refused}");
        let also_built = dir.join("other/target/debug/orq");
        executable(&also_built);
        assert!(restart_target(&built, &also_built).is_err(), "an installed path under target/ is no better");
    }
}

#[cfg(test)]
mod restart_control_tests {
    use super::*;
    use orq_core::remote_inbox::ServerControl;
    use std::sync::{Arc, Mutex};

    fn control(active: Arc<Mutex<Vec<String>>>, target: Result<PathBuf, String>, installed: PathBuf) -> RestartControl {
        RestartControl::new(
            "orq 1.0.0 (aaaaaaa 2026-01-01T00:00:00Z)".into(),
            installed,
            Box::new(move || active.lock().unwrap().clone()),
            Box::new(move || target.clone()),
        )
    }

    fn script(dir: &std::path::Path, version: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("orq");
        let tmp = dir.join("orq.tmp");
        std::fs::write(&tmp, format!("#!/bin/sh\necho '{version}'\n")).unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        path
    }

    #[test]
    fn restart_waits_for_active_runs_and_cancel_stops_it() {
        let active = Arc::new(Mutex::new(vec!["run-1".to_string()]));
        let control = control(active.clone(), Ok(PathBuf::from("/usr/local/bin/orq")), PathBuf::from("/nonexistent/orq"));
        assert!(!control.due(), "nothing was asked");
        assert_eq!(control.restart_when_idle(), Ok(1));
        assert!(control.restart_pending());
        assert_eq!(control.status()["waiting_for"], 1);
        assert!(!control.due(), "a run is active");
        active.lock().unwrap().clear();
        assert!(control.due(), "no run is active any more");
        assert!(control.cancel_restart());
        assert!(!control.due() && !control.restart_pending());
        assert!(!control.cancel_restart(), "nothing is pending now");
    }

    #[test]
    fn restart_is_refused_without_a_binary_to_start() {
        let control = control(Arc::default(), Err("no installed orq".into()), PathBuf::from("/nonexistent/orq"));
        assert_eq!(control.restart_when_idle(), Err("no installed orq".into()));
        assert!(!control.restart_pending());
    }

    #[test]
    fn restart_status_reads_the_installed_version_again_only_when_the_file_changes() {
        let dir = std::env::temp_dir().join(format!("orq-restart-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let installed = script(&dir, "orq 1.0.0 (aaaaaaa 2026-01-01T00:00:00Z)");
        let control = control(Arc::default(), Ok(installed.clone()), installed.clone());
        let status = control.status();
        assert_eq!(status["running"], "orq 1.0.0 (aaaaaaa 2026-01-01T00:00:00Z)");
        assert_eq!(status["installed"], "orq 1.0.0 (aaaaaaa 2026-01-01T00:00:00Z)");
        assert_eq!(status["newer_installed"], false);
        assert_eq!(status["restart_pending"], false);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        script(&dir, "orq 1.1.0 (bbbbbbb 2026-02-01T00:00:00Z)");
        let status = control.status();
        assert_eq!(status["installed"], "orq 1.1.0 (bbbbbbb 2026-02-01T00:00:00Z)");
        assert_eq!(status["newer_installed"], true);
        std::fs::remove_file(&installed).unwrap();
        assert_eq!(control.status()["installed"], serde_json::Value::Null, "no installed orq");
        assert_eq!(control.status()["newer_installed"], false);
    }
}

#[cfg(test)]
mod restart_active_tests {
    use super::*;
    use orq_orchestrator::{Event, RunRecord, RunStore};

    fn run(store: &RunStore, run_id: &str, kinds: &[&str]) {
        store
            .start_run(&RunRecord {
                delivery: Default::default(),
                run_id: run_id.into(),
                project_id: "p".into(),
                feature_id: format!("F-{run_id}"),
                remote: "origin".into(),
                provider: "github".into(),
                target_branch: "main".into(),
                base_sha: "a".repeat(40),
                branch: format!("orq/{run_id}"),
                worktree: PathBuf::from(format!("/nowhere/{run_id}")),
                repository: String::new(),
                profiles: Default::default(),
            })
            .unwrap();
        for (n, kind) in kinds.iter().enumerate() {
            let payload = if *kind == "run.completed" { serde_json::json!({"ending": "review_ready"}) } else { serde_json::json!({}) };
            store
                .append_event(&Event {
                    schema_version: 1,
                    event_id: format!("{run_id}-e{n}"),
                    run_id: run_id.into(),
                    project_id: "p".into(),
                    feature_id: format!("F-{run_id}"),
                    kind: kind.to_string(),
                    stage: Some("gate".into()),
                    attempt: 1,
                    occurred_at: format!("2026-01-01T00:00:0{n}Z"),
                    payload,
                })
                .unwrap();
        }
    }

    #[test]
    fn restart_waits_for_running_and_pausing_runs_but_not_paused_or_ended_ones() {
        let dir = std::env::temp_dir().join(format!("orq-restart-active-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = RunStore::new(&dir);
        run(&store, "run-gate", &["stage.started"]);
        run(&store, "run-pausing", &["stage.started", "run.pause_requested"]);
        run(&store, "run-paused", &["stage.started", "run.pause_requested", "run.paused"]);
        run(&store, "run-done", &["stage.started", "run.completed"]);
        let mut active = active_run_ids(&dir).unwrap();
        active.sort();
        assert_eq!(active, vec!["run-gate".to_string(), "run-pausing".to_string()]);
    }

    fn fresh(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("orq-restart-unreadable-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn unreadable_restart_state_is_an_error_naming_the_run_never_an_idle_server() {
        let dir = fresh("record");
        run(&RunStore::new(&dir), "run-ok", &["run.completed"]);
        std::fs::create_dir_all(dir.join("runs/run-bad")).unwrap();
        std::fs::write(dir.join("runs/run-bad/run.json"), "{ not json").unwrap();
        let why = active_run_ids(&dir).unwrap_err();
        assert!(why.contains("run-bad") && why.contains("unreadable"), "{why}");

        let dir = fresh("events");
        let store = RunStore::new(&dir);
        run(&store, "run-torn", &["stage.started"]);
        let events = dir.join("runs/run-torn/events.jsonl");
        let mut text = std::fs::read_to_string(&events).unwrap();
        text.push_str("{ torn\n");
        std::fs::write(&events, text).unwrap();
        let why = active_run_ids(&dir).unwrap_err();
        assert!(why.contains("run-torn") && why.contains("unreadable"), "{why}");
    }

    #[test]
    fn a_pending_restart_over_unreadable_runs_waits_and_counts_the_error_as_a_run() {
        use orq_core::remote_inbox::ServerControl;
        let base = fresh("control");
        let roots = orq_core::storage::StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
        std::fs::create_dir_all(roots.state.join("runs/run-bad")).unwrap();
        std::fs::write(roots.state.join("runs/run-bad/run.json"), "{ not json").unwrap();
        let control = restart_control(&roots);
        control.pending.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(!control.due(), "a run it cannot read may be active");
        assert!(control.restart_pending());
        assert_eq!(control.status()["waiting_for"], 1, "{}", control.status());
    }
}

#[cfg(test)]
mod restart_build_tests {
    #[test]
    fn restart_build_names_the_version_commit_and_build_time() {
        let build = super::BUILD;
        let rest = build.strip_prefix(concat!(env!("CARGO_PKG_VERSION"), " (")).unwrap_or_else(|| panic!("{build}"));
        let (commit, time) = rest.strip_suffix(')').and_then(|rest| rest.split_once(' ')).unwrap_or_else(|| panic!("{build}"));
        assert!(commit == "unknown" || (commit.len() >= 7 && commit.chars().all(|c| c.is_ascii_hexdigit())), "{build}");
        assert!(time.len() == 20 && time.ends_with('Z') && time.as_bytes()[10] == b'T', "{build}");
    }
}
