//! `orq remote serve`: starts the review inbox server for one repository.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::{fail, usage_error};

pub fn serve(args: &[OsString]) -> ExitCode {
    let mut repo = None;
    let mut listen = String::from("127.0.0.1:7878");
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
            _ => return usage_error(),
        }
    }
    let Some(repo) = repo else { return usage_error() };
    let settings = match orq_orchestrator::load_run_settings(&repo, &|name| std::env::var(name).ok()) {
        Ok(settings) => settings,
        Err(err) => return fail(format!("cannot read the run settings: {err}")),
    };
    if settings.remote_control != Some(true) {
        return fail("remote control is off; set remote_control = true to serve the review inbox");
    }
    let roots = match orq_core::storage::StorageRoots::resolve(&|name| std::env::var(name).ok()) {
        Ok(roots) => roots,
        Err(err) => return fail(format!("cannot locate the Orq directories: {err}")),
    };
    let config = match orq_core::remote_inbox::RemoteConfig::load(&roots) {
        Ok(config) => config,
        Err(reason) => return fail(reason),
    };
    let project = match orq_git::resolve_project(&repo) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    let listener = match std::net::TcpListener::bind(&listen) {
        Ok(listener) => listener,
        Err(err) => return fail(format!("cannot listen on {listen}: {:?}", err.kind())),
    };
    let state = std::sync::Arc::new(orq_remote::ServerState {
        roots,
        config,
        project_id: crate::pipeline::project_id(&project),
        project_name: project.name.clone(),
    });
    let server = match orq_remote::Server::start(listener, state) {
        Ok(server) => server,
        Err(err) => return fail(format!("cannot start the server: {:?}", err.kind())),
    };
    println!("orq remote: listening on http://{}", server.addr());
    let _ = std::io::Write::flush(&mut std::io::stdout());
    wait_for_interrupt();
    drop(server);
    ExitCode::SUCCESS
}

static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn note_interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Blocks until Ctrl-C (or a terminate signal) arrives.
fn wait_for_interrupt() {
    // SAFETY: the handler only stores to an atomic flag.
    unsafe {
        libc::signal(libc::SIGINT, note_interrupt as libc::sighandler_t);
        libc::signal(libc::SIGTERM, note_interrupt as libc::sighandler_t);
    }
    while !INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
