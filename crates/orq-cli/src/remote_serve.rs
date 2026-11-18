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
    run(args)
}

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
    let mut projects = orq_remote::projects::StoreProjects::new(roots.clone(), scan_root);
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
    println!("orq remote: listening on http://{}", server.addr());
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
    // Queued features start on their own; no restart is ever pending yet, so starts are never held for one.
    let queue_rounds = orq_remote::projects::start_queue_rounds(queued, std::time::Duration::from_secs(5), || false);
    wait_for_interrupt();
    drop(queue_rounds);
    drop(repair_rounds);
    drop(server);
    drop(rounds);
    ExitCode::SUCCESS
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
    }
}

/// Blocks until a signal has arrived.
fn wait_for_interrupt() {
    while !INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
