//! `orq remote serve`: starts the review inbox server for one repository.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::{fail, usage_error};

/// Where a plain `orq serve` listens unless told otherwise: this machine only.
const DEFAULT_LISTEN: &str = "127.0.0.1:7878";

/// `orq serve [--repo <path>] [--listen <addr:port>] [--passkey-host <name> [--no-loopback-token]]`: the same server with nothing to set up. The
/// command itself is the opt-in, so the run settings need not enable remote control, a missing
/// token is created, and the repository defaults to the current directory.
pub fn serve_plain(args: &[OsString]) -> ExitCode {
    run(args, false)
}

pub fn serve(args: &[OsString]) -> ExitCode {
    run(args, true)
}

fn run(args: &[OsString], strict: bool) -> ExitCode {
    let mut repo = None;
    let mut listen = String::from(DEFAULT_LISTEN);
    let mut passkey_host = None;
    let mut loopback_token = true;
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
        None if !strict => match std::env::current_dir() {
            Ok(dir) => dir,
            Err(err) => return fail(format!("cannot read the current directory: {:?}", err.kind())),
        },
        None => return usage_error(),
    };
    let settings = match orq_orchestrator::load_run_settings(&repo, &|name| std::env::var(name).ok()) {
        Ok(settings) => settings,
        Err(err) => return fail(format!("cannot read the run settings: {err}")),
    };
    if strict && settings.remote_control != Some(true) {
        return fail("remote control is off; set remote_control = true to serve the review inbox");
    }
    let roots = match orq_core::storage::StorageRoots::resolve(&|name| std::env::var(name).ok()) {
        Ok(roots) => roots,
        Err(err) => return fail(format!("cannot locate the Orq directories: {err}")),
    };
    let loaded = if strict { orq_core::remote_inbox::RemoteConfig::load(&roots) } else { orq_core::remote_inbox::RemoteConfig::load_or_create(&roots) };
    let config = match loaded {
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
    let projects = std::sync::Arc::new(orq_remote::projects::StoreProjects::new(roots.clone(), scan_root));
    let state_roots = roots.clone();
    let state = std::sync::Arc::new(orq_remote::ServerState {
        roots,
        config,
        project_id: crate::pipeline::project_id(&project),
        project_name: project.name.clone(),
    });
    let started = match auth.clone() {
        Some(auth) => orq_remote::Server::start_with_auth(listener, state, projects, auth),
        None => orq_remote::Server::start_with_projects(listener, state, projects),
    };
    let server = match started {
        Ok(server) => server,
        Err(err) => return fail(format!("cannot start the server: {:?}", err.kind())),
    };
    println!("orq remote: listening on http://{}", server.addr());
    if let Some(auth) = &auth {
        println!("passkeys: sign in at {} (serve it over HTTPS, for example `tailscale serve --bg {}`)", auth.host().origin(), server.addr().port());
    }
    if !strict {
        let token = orq_core::remote_inbox::RemoteConfig::token_path(&state_roots);
        println!("sign in with the token stored in {}", token.display());
        println!("read it with: cat -- {}", shell_quote(&token.display().to_string()));
    }
    let _ = std::io::Write::flush(&mut std::io::stdout());
    wait_for_interrupt();
    drop(server);
    ExitCode::SUCCESS
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
