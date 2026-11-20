//! Web push where things happen: a run's events, its review, its project's queue and the server's restart each
//! send their kind to the subscribed devices, off the run's path, and the worker reads the words to show back.

mod web_deps;
mod web_push_session;

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use serde_json::{json, Value};

/// A push service on loopback that records each request; with `hang`, it reads the request and never answers.
struct FakePush {
    port: u16,
    seen: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl FakePush {
    fn start(hang: bool) -> FakePush {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<Vec<u8>>>> = Default::default();
        let record = seen.clone();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 {
                        break;
                    }
                    let header = header.trim_end();
                    if header.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0u8; length];
                let _ = reader.read_exact(&mut body);
                record.lock().unwrap().push(body);
                if hang {
                    held.push(stream);
                    continue;
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
            }
        });
        FakePush { port, seen }
    }

    fn bodies(&self) -> Vec<Vec<u8>> {
        self.seen.lock().unwrap().clone()
    }
}

/// A browser's side of a subscription: its keys, so a test can read what was sent to it.
struct Device {
    secret: p256::SecretKey,
    auth: [u8; 16],
}

impl Device {
    fn new() -> Device {
        Device {
            secret: p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng),
            auth: [7u8; 16],
        }
    }

    /// RFC 8291 decryption of an aes128gcm body.
    fn decrypt(&self, body: &[u8]) -> Value {
        use aes_gcm::aead::Aead;
        use aes_gcm::KeyInit;
        let (salt, rest) = body.split_at(16);
        let (_size, rest) = rest.split_at(4);
        let (id_len, rest) = (rest[0] as usize, &rest[1..]);
        let (sender_public, cipher) = rest.split_at(id_len);
        let sender = p256::PublicKey::from_sec1_bytes(sender_public).unwrap();
        let shared =
            p256::ecdh::diffie_hellman(self.secret.to_nonzero_scalar(), sender.as_affine());
        let own_public = self.secret.public_key().to_encoded_point(false);
        let mut info = b"WebPush: info\0".to_vec();
        info.extend_from_slice(own_public.as_bytes());
        info.extend_from_slice(sender_public);
        let mut ikm = [0u8; 32];
        hkdf::Hkdf::<sha2::Sha256>::new(Some(&self.auth), shared.raw_secret_bytes())
            .expand(&info, &mut ikm)
            .unwrap();
        let prk = hkdf::Hkdf::<sha2::Sha256>::new(Some(salt), &ikm);
        let (mut key, mut nonce) = ([0u8; 16], [0u8; 12]);
        prk.expand(b"Content-Encoding: aes128gcm\0", &mut key)
            .unwrap();
        prk.expand(b"Content-Encoding: nonce\0", &mut nonce)
            .unwrap();
        let mut plain = aes_gcm::Aes128Gcm::new(&key.into())
            .decrypt(&nonce.into(), cipher)
            .unwrap();
        assert_eq!(plain.pop(), Some(2));
        serde_json::from_slice(&plain).unwrap()
    }
}

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
    repo: PathBuf,
    service: FakePush,
    device: Device,
}

/// Storage, a project `alpha` in the scan root, and one device subscribed at a push service.
fn setup(label: &str, hang: bool) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots {
        data: base.join("data"),
        config: base.join("config"),
        state: base.join("state"),
        cache: base.join("cache"),
    };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let scan = base.join("code");
    let repo = scan.join("alpha");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "start",
        ],
    ] {
        let out = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let repo = std::fs::canonicalize(repo).unwrap();
    let (service, device) = (FakePush::start(hang), Device::new());
    let public = device.secret.public_key().to_encoded_point(false);
    let store = orq_core::store::Store::open(&roots.database()).unwrap();
    let subscription = orq_core::store::push::NewSubscription {
        endpoint: format!("http://127.0.0.1:{}/push/one", service.port),
        p256dh: orq_remote::web_push::base64url_encode(public.as_bytes()),
        auth: orq_remote::web_push::base64url_encode(&device.auth),
        label: "phone".into(),
    };
    store.add_push_subscription(&subscription, 1).unwrap();
    Setup {
        scan,
        roots,
        repo,
        service,
        device,
    }
}

fn provider(s: &Setup) -> StoreProjects {
    StoreProjects::new(s.roots.clone(), s.scan.clone())
}

/// A run of `feature` in `alpha`, started.
fn seed_run(s: &Setup, run: &str, feature: &str) {
    let record = RunRecord {
        delivery: Default::default(),
        run_id: run.into(),
        project_id: orq_git::project_id(&orq_git::resolve_project(&s.repo).unwrap()),
        feature_id: feature.into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: "a".repeat(40),
        branch: format!("orq/{run}"),
        worktree: s.repo.join(".claude/worktrees").join(run),
        repository: String::new(),
        profiles: Default::default(),
    };
    RunStore::new(&s.roots.state).create(&record).unwrap();
}

/// Records `kind` on `run`; the event's id.
fn event(s: &Setup, run: &str, kind: &str, payload: Value) -> String {
    let store = RunStore::new(&s.roots.state);
    let record = store.load(run).unwrap();
    let n = store.events(run).unwrap().len();
    let event_id = format!("{run}-{n}");
    let event = Event {
        schema_version: 1,
        event_id: event_id.clone(),
        run_id: run.into(),
        project_id: record.project_id,
        feature_id: record.feature_id,
        kind: kind.into(),
        stage: None,
        attempt: 1,
        occurred_at: "2026-11-20T10:00:00Z".into(),
        payload,
    };
    store.append_event(&event).unwrap();
    event_id
}

/// One look at what the runs recorded; waits for its pushes and gives how many devices each reached.
fn look(projects: &StoreProjects, watch: &mut orq_remote::web_push::RunWatch) -> Vec<usize> {
    projects
        .push_round(watch)
        .into_iter()
        .map(|send| send.join().unwrap())
        .collect()
}

#[test]
fn a_needs_input_event_sends_one_push_with_only_the_four_ids_and_a_second_within_five_minutes_sends_none(
) {
    let s = setup("dispatch-needs", false);
    let projects = provider(&s);
    seed_run(&s, "run-1", "COM-01");
    event(&s, "run-1", "stage.started", json!({}));
    let mut watch = orq_remote::web_push::RunWatch::new(&s.roots);
    // What was recorded before the watch began is never sent.
    assert!(look(&projects, &mut watch).is_empty());

    let asked = event(
        &s,
        "run-1",
        "run.needs_input",
        json!({"question": "which database should it use?"}),
    );
    assert_eq!(look(&projects, &mut watch), vec![1]);
    let bodies = s.service.bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        s.device.decrypt(&bodies[0]),
        json!({"kind": "needs_input", "project": "alpha", "run": "run-1", "event": asked})
    );

    event(&s, "run-1", "run.input_given", json!({}));
    event(
        &s,
        "run-1",
        "run.needs_input",
        json!({"question": "and the port?"}),
    );
    assert_eq!(
        look(&projects, &mut watch),
        vec![0],
        "the same run and kind inside five minutes"
    );
    assert_eq!(s.service.bodies().len(), 1);
}

/// Waits up to five seconds for the device to have been sent `n` pushes, then a moment more for any extra one.
fn wait_for(s: &Setup, n: usize) -> Vec<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while s.service.bodies().len() < n && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    kinds_sent(s)
}

/// Stores `run`'s review at github as the review refresh last saw it.
fn review(
    s: &Setup,
    run: &str,
    state: orq_core::review_lifecycle::ProviderState,
    observed_head: Option<&str>,
) {
    let mut lifecycle = orq_core::review_lifecycle::ReviewLifecycle::associate(
        "github",
        "12",
        "https://github.com/o/alpha/pull/12",
        &"a".repeat(40),
    );
    lifecycle.state = state;
    lifecycle.observed = true;
    lifecycle.stale = observed_head.is_some();
    lifecycle.observed_head_sha = observed_head.map(str::to_string);
    orq_core::store::Store::open(&s.roots.database())
        .unwrap()
        .save_lifecycle(run, &lifecycle)
        .unwrap();
}

#[test]
fn a_review_that_is_ready_or_stale_after_a_push_and_an_mr_merged_each_send_their_kind_once() {
    use orq_core::review_lifecycle::ProviderState;
    let s = setup("dispatch-review", false);
    let projects = provider(&s);
    seed_run(&s, "run-1", "COM-01");
    seed_run(&s, "run-2", "COM-02");
    let mut watch = orq_remote::web_push::RunWatch::new(&s.roots);

    event(
        &s,
        "run-1",
        "run.completed",
        json!({"ending": "review_ready"}),
    );
    assert_eq!(look(&projects, &mut watch), vec![1]);
    assert_eq!(kinds_sent(&s), ["review"]);

    // A push to the MR moved its head past the report: stale, told once however often the refresh looks.
    review(&s, "run-2", ProviderState::Open, Some(&"c".repeat(40)));
    projects.review_looked_at("run-2");
    projects.review_looked_at("run-2");
    assert_eq!(wait_for(&s, 2), ["review", "review"]);
    let stale = s.device.decrypt(&s.service.bodies()[1]);
    assert_eq!(
        (stale["project"].as_str(), stale["run"].as_str()),
        (Some("alpha"), Some("run-2"))
    );

    review(&s, "run-1", ProviderState::Merged, None);
    projects.review_looked_at("run-1");
    projects.review_looked_at("run-1");
    assert_eq!(
        wait_for(&s, 3),
        ["review", "review", "merged"],
        "merged once"
    );
    // An open review that is not stale is no news.
    review(&s, "run-1", ProviderState::Open, None);
    projects.review_looked_at("run-1");
    assert_eq!(wait_for(&s, 4).len(), 3);
}

/// Starts a run the way `orq run` records one.
struct Launcher {
    state: PathBuf,
    started: Mutex<usize>,
}

impl orq_remote::projects::RunLauncher for Launcher {
    fn start(
        &self,
        repo: &std::path::Path,
        feature: &str,
    ) -> Result<String, orq_remote::projects::LaunchError> {
        let mut started = self.started.lock().unwrap();
        *started += 1;
        let run = format!("run-{started}-{feature}");
        let record = RunRecord {
            delivery: Default::default(),
            run_id: run.clone(),
            project_id: orq_git::project_id(&orq_git::resolve_project(repo).unwrap()),
            feature_id: feature.into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "a".repeat(40),
            branch: format!("orq/{run}"),
            worktree: repo.join(".claude/worktrees").join(&run),
            repository: String::new(),
            profiles: Default::default(),
        };
        RunStore::new(&self.state).create(&record).unwrap();
        Ok(run)
    }

    fn start_unattended(
        &self,
        repo: &std::path::Path,
        feature: &str,
        _profiles: Option<&orq_core::remote_inbox::LaunchProfiles>,
    ) -> Result<String, orq_remote::projects::LaunchError> {
        self.start(repo, feature)
    }
}

/// `alpha`'s queue holding `features`, under `settings`, worked by a launcher that only records its runs.
fn queued(s: &Setup, features: &[&str], settings: Value) -> StoreProjects {
    let ledger: Vec<Value> = features
        .iter()
        .map(|name| json!({"featureName": name, "dependsOn": [], "status": "pending"}))
        .collect();
    std::fs::write(
        s.repo.join(".tdd-features.json"),
        Value::Array(ledger).to_string(),
    )
    .unwrap();
    let projects = provider(s).with_launcher(Arc::new(Launcher {
        state: s.roots.state.clone(),
        started: Mutex::new(0),
    }));
    let send = |method: &str, path: &str, body: Value| {
        let config = orq_core::remote_inbox::RemoteConfig {
            token: "tok".into(),
        };
        let request = orq_core::remote_inbox::Request {
            method,
            path,
            peer: "127.0.0.1".parse().unwrap(),
            authorization: Some("Bearer tok"),
        };
        let r = orq_core::remote_inbox::handle_with_body(
            &s.roots,
            &config,
            "server-project",
            "server",
            &request,
            body.to_string().as_bytes(),
            &projects,
        );
        assert!(r.status < 300, "{} {path}: {}", r.status, r.body);
        r.body
    };
    let current = send("GET", "/api/projects/alpha/settings", Value::Null);
    let mut body = json!({"implementer": null, "reviewer": null, "delivery": "review", "max_active_runs": 1, "min_free_disk_gb": null, "revision": current["settings"]["revision"]});
    for (key, value) in settings.as_object().unwrap() {
        body[key] = value.clone();
    }
    send("POST", "/api/projects/alpha/settings", body);
    send(
        "POST",
        "/api/projects/alpha/queue",
        json!({"features": features}),
    );
    projects
}

fn round(projects: &StoreProjects) {
    projects.queue_round(&orq_remote::projects::QueueRound {
        minute_of_day: 12 * 60,
        restart_pending: false,
    });
}

#[test]
fn a_queue_pausing_on_a_failure_or_a_review_and_a_queue_running_out_each_send_one_push() {
    let s = setup("dispatch-queue-failure", false);
    let projects = queued(
        &s,
        &["COM-01", "COM-02"],
        json!({"queue_stop_on_failure": true}),
    );
    round(&projects);
    event(
        &s,
        "run-1-COM-01",
        "run.completed",
        json!({"ending": "blocked"}),
    );
    round(&projects);
    round(&projects);
    assert_eq!(
        wait_for(&s, 1),
        ["queue_paused"],
        "one push, however many rounds see the pause"
    );
    let paused = s.device.decrypt(&s.service.bodies()[0]);
    assert_eq!(
        (paused["project"].as_str(), paused["run"].as_str()),
        (Some("alpha"), None)
    );

    let s = setup("dispatch-queue-review", false);
    let projects = queued(
        &s,
        &["COM-01", "COM-02"],
        json!({"queue_wait_for_review": true}),
    );
    round(&projects);
    event(
        &s,
        "run-1-COM-01",
        "run.completed",
        json!({"ending": "review_ready"}),
    );
    review(
        &s,
        "run-1-COM-01",
        orq_core::review_lifecycle::ProviderState::Open,
        None,
    );
    round(&projects);
    round(&projects);
    assert_eq!(
        wait_for(&s, 1),
        ["queue_paused"],
        "the queue holds COM-02 for COM-01's review"
    );

    let s = setup("dispatch-queue-finished", false);
    let projects = queued(&s, &["COM-01"], json!({}));
    round(&projects);
    assert!(
        wait_for(&s, 1).is_empty(),
        "a queue with a run going is not finished"
    );
    event(
        &s,
        "run-1-COM-01",
        "run.completed",
        json!({"ending": "merged"}),
    );
    round(&projects);
    round(&projects);
    assert_eq!(wait_for(&s, 1), ["queue_finished"]);
}

#[test]
fn a_push_endpoint_that_hangs_does_not_delay_the_runs_events_and_is_given_up_after_ten_seconds() {
    let s = setup("dispatch-hang", true);
    let projects = provider(&s);
    seed_run(&s, "run-1", "COM-01");
    let mut watch = orq_remote::web_push::RunWatch::new(&s.roots);
    event(&s, "run-1", "run.needs_input", json!({}));
    let began = std::time::Instant::now();
    let sends = projects.push_round(&mut watch);
    assert!(
        began.elapsed() < std::time::Duration::from_secs(2),
        "the look waited for the push: {:?}",
        began.elapsed()
    );
    // The run carries on recording while its push hangs.
    event(&s, "run-1", "run.input_given", json!({}));
    event(&s, "run-1", "run.failed", json!({"reason": "x"}));
    assert_eq!(
        RunStore::new(&s.roots.state).events("run-1").unwrap().len(),
        3
    );
    assert!(began.elapsed() < std::time::Duration::from_secs(2));
    let reached: Vec<usize> = sends.into_iter().map(|send| send.join().unwrap()).collect();
    assert_eq!(
        reached,
        vec![0],
        "a push that never answers reached no device"
    );
    let waited = began.elapsed();
    assert!(
        waited >= std::time::Duration::from_secs(9) && waited < std::time::Duration::from_secs(15),
        "given up after {waited:?}"
    );
}

/// `count` runs, each just recorded as needing input after the watch began; the watch.
fn many_needing_input(s: &Setup, count: usize) -> orq_remote::web_push::RunWatch {
    let runs: Vec<String> = (0..count).map(|n| format!("run-{n}")).collect();
    for run in &runs {
        seed_run(s, run, "COM-01");
    }
    let watch = orq_remote::web_push::RunWatch::new(&s.roots);
    for run in &runs {
        event(s, run, "run.needs_input", json!({}));
    }
    watch
}

#[test]
fn many_pushes_to_a_hanging_endpoint_wait_on_a_few_sends_at_a_time_and_a_burst_to_a_healthy_one_all_arrive() {
    let s = setup("dispatch-bounded", true);
    let projects = provider(&s);
    let mut watch = many_needing_input(&s, 40);
    let began = std::time::Instant::now();
    let sends = projects.push_round(&mut watch);
    assert!(began.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(sends.len(), 40, "each push is taken to be sent");
    std::thread::sleep(std::time::Duration::from_secs(2));
    let waiting = s.service.bodies().len();
    assert!(
        waiting > 0 && waiting <= orq_remote::web_push::SENDS_IN_FLIGHT,
        "{waiting} sends waiting on the service at once"
    );

    let s = setup("dispatch-burst", false);
    let projects = provider(&s);
    let mut watch = many_needing_input(&s, 3 * orq_remote::web_push::SENDS_IN_FLIGHT);
    let reached = look(&projects, &mut watch);
    assert_eq!(
        reached,
        vec![1; 3 * orq_remote::web_push::SENDS_IN_FLIGHT],
        "none of a burst is dropped"
    );
    assert_eq!(
        s.service.bodies().len(),
        3 * orq_remote::web_push::SENDS_IN_FLIGHT
    );
}

#[test]
fn a_database_held_by_another_writer_does_not_delay_the_look_and_the_push_goes_out_once_it_is_free() {
    let s = setup("dispatch-locked", false);
    let projects = provider(&s);
    seed_run(&s, "run-1", "COM-01");
    let mut watch = orq_remote::web_push::RunWatch::new(&s.roots);
    event(&s, "run-1", "run.needs_input", json!({}));
    let held = rusqlite::Connection::open(s.roots.database()).unwrap();
    held.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let began = std::time::Instant::now();
    let sends = projects.push_round(&mut watch);
    assert!(
        began.elapsed() < std::time::Duration::from_secs(1),
        "the look waited for the database: {:?}",
        began.elapsed()
    );
    assert_eq!(sends.len(), 1);
    std::thread::sleep(std::time::Duration::from_secs(2));
    held.execute_batch("COMMIT").unwrap();
    let reached: Vec<usize> = sends.into_iter().map(|send| send.join().unwrap()).collect();
    assert_eq!(reached, vec![1], "claimed and sent once the writer let go");
}

#[test]
fn serve_pushes_its_restart_once_then_keeps_pushing_what_runs_record_until_its_rounds_stop() {
    let s = setup("dispatch-rounds", false);
    seed_run(&s, "run-1", "COM-01");
    event(&s, "run-1", "run.needs_input", json!({}));
    let rounds = orq_remote::projects::start_push_rounds(
        Arc::new(provider(&s)),
        std::time::Duration::from_millis(20),
        true,
    );
    assert_eq!(
        wait_for(&s, 1),
        ["restarted"],
        "what was recorded before serve started is not news"
    );
    let restarted = s.device.decrypt(&s.service.bodies()[0]);
    assert_eq!(
        (restarted["run"].as_str(), restarted["event"].as_str()),
        (None, Some("restarted"))
    );
    event(&s, "run-1", "run.failed", json!({"reason": "x"}));
    assert_eq!(wait_for(&s, 2), ["restarted", "failed"]);
    drop(rounds);
    event(&s, "run-1", "run.completed", json!({"ending": "merged"}));
    assert_eq!(wait_for(&s, 3).len(), 2, "stopped rounds send nothing");

    // A server started fresh, not by a restart, does not say it restarted.
    let s = setup("dispatch-fresh", false);
    let rounds = orq_remote::projects::start_push_rounds(
        Arc::new(provider(&s)),
        std::time::Duration::from_millis(20),
        false,
    );
    assert!(wait_for(&s, 1).is_empty());
    drop(rounds);
}

/// Serves `s`'s projects with passkey sign-in, where the loopback peer gets nothing without credentials.
fn serve(s: &Setup) -> orq_remote::Server {
    let state = Arc::new(orq_remote::ServerState {
        roots: s.roots.clone(),
        config: orq_core::remote_inbox::RemoteConfig {
            token: "tok".into(),
        },
        project_id: "server-project".into(),
        project_name: "server".into(),
    });
    let host = orq_core::passkey_register::PasskeyHost::from_setting(web_push_session::RP).unwrap();
    let auth =
        Arc::new(orq_core::passkey_routes::PasskeyAuth::new(host).with_loopback_token(false));
    orq_remote::Server::start_with_auth(
        std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
        state,
        Arc::new(provider(s)),
        auth,
    )
    .unwrap()
}

fn text_of(
    server: &orq_remote::Server,
    session: Option<&web_push_session::Session>,
    query: &str,
) -> (u16, Value) {
    let headers: Vec<(&str, &str)> = session
        .map(|s| vec![("Cookie", s.cookie.as_str())])
        .unwrap_or_default();
    let (status, _, body) = web_push_session::raw(
        server.addr(),
        "GET",
        &format!("/api/push/text?{query}"),
        &headers,
        b"",
    );
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

#[test]
fn push_text_hides_secrets_in_a_question_or_reason_and_cuts_long_text_at_two_hundred_characters() {
    let s = setup("dispatch-text-redacted", false);
    seed_run(&s, "run-1", "COM-01");
    let token = format!("ghp_{}", "A1b2C3d4".repeat(4));
    let asked = event(
        &s,
        "run-1",
        "run.needs_input",
        json!({"question": format!("push with {token} or password=hunter2hunter2?")}),
    );
    let failed = event(
        &s,
        "run-1",
        "run.failed",
        json!({"reason": "curl -H 'Authorization: Bearer abcdefgh12345678' https://ci:s3cretpass@ci.example/x refused"}),
    );
    // Multibyte text far past the cut, with its end in sight only if the cut is by bytes or missing.
    let long = event(
        &s,
        "run-1",
        "run.failed",
        json!({"reason": format!("{}THE-END", "é漢".repeat(300))}),
    );
    let server = serve(&s);
    let session = web_push_session::sign_in(server.addr(), "tok");
    let body_of = |event: &str| {
        let (status, body) = text_of(
            &server,
            Some(&session),
            &format!("project=alpha&run=run-1&event={event}"),
        );
        assert_eq!(status, 200, "{body}");
        body["body"].as_str().unwrap().to_string()
    };

    let body = body_of(&asked);
    assert!(
        !body.contains(&token) && !body.contains("hunter2hunter2"),
        "{body}"
    );
    assert!(body.contains("[REDACTED]") && body.contains("push with"), "{body}");
    let body = body_of(&failed);
    assert!(
        !body.contains("abcdefgh12345678") && !body.contains("s3cretpass"),
        "{body}"
    );
    assert!(body.contains("[REDACTED]") && body.contains("refused"), "{body}");
    let body = body_of(&long);
    assert_eq!(body.chars().count(), 200, "{body}");
    assert_eq!(body, "é漢".repeat(100));
}

#[test]
fn push_text_gives_each_kinds_title_body_and_page_to_a_signed_in_session_and_404_for_what_it_cannot_see(
) {
    let s = setup("dispatch-text", false);
    seed_run(&s, "run-1", "COM-01");
    let asked = event(
        &s,
        "run-1",
        "run.needs_input",
        json!({"question": "which port?"}),
    );
    let failed = event(
        &s,
        "run-1",
        "run.failed",
        json!({"reason": "the gate failed"}),
    );
    let finished = event(&s, "run-1", "run.completed", json!({"ending": "merged"}));
    let started = event(&s, "run-1", "run.launched", json!({}));
    let ready = event(
        &s,
        "run-1",
        "run.completed",
        json!({"ending": "review_ready"}),
    );
    let quiet = event(&s, "run-1", "stage.started", json!({}));
    // A run of a project serve does not show.
    let elsewhere = web_deps::private_temp_dir("dispatch-text-elsewhere");
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(&elsewhere)
        .output()
        .unwrap();
    assert!(out.status.success());
    let mut record = RunStore::new(&s.roots.state).load("run-1").unwrap();
    record.run_id = "run-9".into();
    record.project_id = orq_git::project_id(&orq_git::resolve_project(&elsewhere).unwrap());
    RunStore::new(&s.roots.state).create(&record).unwrap();
    let hidden = event(&s, "run-9", "run.needs_input", json!({}));

    let server = serve(&s);
    assert_eq!(
        text_of(
            &server,
            None,
            &format!("project=alpha&run=run-1&event={asked}")
        )
        .0,
        401,
        "a session is needed"
    );
    let session = web_push_session::sign_in(server.addr(), "tok");
    // A read needs only the session's cookie, not the CSRF token a change sends.
    assert!(!session.csrf.is_empty());
    let read = |query: String| {
        let (status, body) = text_of(&server, Some(&session), &query);
        assert_eq!(status, 200, "{query}: {body}");
        (
            body["title"].as_str().unwrap().to_string(),
            body["body"].as_str().unwrap().to_string(),
            body["path"].as_str().unwrap().to_string(),
        )
    };
    let run_page = "/projects/alpha/orchestrations/run-1";
    let (title, body, path) = read(format!("project=alpha&run=run-1&event={asked}"));
    assert!(
        title.contains("COM-01") && title.contains("input"),
        "{title}"
    );
    assert!(body.contains("which port?"), "{body}");
    assert_eq!(path, run_page);
    let (title, body, path) = read(format!("project=alpha&run=run-1&event={failed}"));
    assert!(
        title.contains("COM-01") && title.contains("failed") && body.contains("the gate failed"),
        "{title} / {body}"
    );
    assert_eq!(path, run_page);
    let (title, _, path) = read(format!("project=alpha&run=run-1&event={finished}"));
    assert!(title.contains("finished"), "{title}");
    assert_eq!(path, run_page);
    let (title, _, path) = read(format!("project=alpha&run=run-1&event={started}"));
    assert!(title.contains("started"), "{title}");
    assert_eq!(path, run_page);
    let (title, _, path) = read(format!("project=alpha&run=run-1&event={ready}"));
    assert!(title.contains("review"), "{title}");
    assert_eq!(path, "/projects/alpha/reviews/run-1");
    let (title, _, path) = read("project=alpha&run=run-1&event=stale%3Acccc".into());
    assert!(title.contains("review"), "{title}");
    assert_eq!(path, "/projects/alpha/reviews/run-1");
    let (title, _, path) = read("project=alpha&run=run-1&event=merged".into());
    assert!(title.contains("merged"), "{title}");
    assert_eq!(path, "/projects/alpha/reviews/run-1");
    let (title, _, path) = read("project=alpha&event=queue_paused".into());
    assert!(
        title.contains("alpha") && title.contains("paused"),
        "{title}"
    );
    assert_eq!(path, "/projects/alpha/queue");
    let (title, _, path) = read("project=alpha&event=queue_paused%3Arun-1".into());
    assert!(title.contains("paused"), "{title}");
    assert_eq!(path, "/projects/alpha/queue");
    let (title, _, path) = read("project=alpha&event=queue_finished".into());
    assert!(title.contains("finished"), "{title}");
    assert_eq!(path, "/projects/alpha/queue");
    let (title, _, path) = read("project=server&event=restarted".into());
    assert!(title.contains("restarted"), "{title}");
    assert_eq!(path, "/dashboard");
    // The Queue page a push opens is one the page draws.
    assert_eq!(
        web_push_session::raw(
            server.addr(),
            "GET",
            "/projects/alpha/queue",
            &[("Cookie", &session.cookie)],
            b""
        )
        .0,
        200
    );

    for query in [
        format!("project=alpha&run=run-9&event={hidden}"),
        format!("project=alpha&run=run-1&event={quiet}"),
        "project=alpha&run=run-1&event=nothing".into(),
        "project=alpha&run=run-404&event=merged".into(),
        "project=nowhere&event=queue_paused".into(),
        "project=alpha&event=restarted".into(),
        format!("project=alpha&event={asked}"),
        "run=run-1&event=merged".into(),
    ] {
        assert_eq!(text_of(&server, Some(&session), &query).0, 404, "{query}");
    }
}

#[test]
fn an_event_half_written_when_the_watch_begins_is_sent_once_it_is_whole() {
    use std::io::Write;
    let s = setup("dispatch-partial", false);
    let projects = provider(&s);
    seed_run(&s, "run-1", "COM-01");
    event(&s, "run-1", "stage.started", json!({}));
    let record = RunStore::new(&s.roots.state).load("run-1").unwrap();
    let asked = Event { schema_version: 1, event_id: "run-1-asked".into(), run_id: "run-1".into(), project_id: record.project_id, feature_id: record.feature_id, kind: "run.needs_input".into(), stage: None, attempt: 1, occurred_at: "2026-11-20T10:00:00Z".into(), payload: json!({}) };
    let line = format!("{}\n", serde_json::to_string(&asked).unwrap());
    let (head, tail) = line.split_at(line.len() / 2);
    let file = s.roots.state.join("runs").join("run-1").join("events.jsonl");
    let mut events = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
    events.write_all(head.as_bytes()).unwrap();
    // The server starts while the run is part way through the line.
    let mut watch = orq_remote::web_push::RunWatch::new(&s.roots);
    events.write_all(tail.as_bytes()).unwrap();
    drop(events);
    assert_eq!(look(&projects, &mut watch), vec![1]);
    assert_eq!(s.device.decrypt(&s.service.bodies()[0])["event"], "run-1-asked");
}

/// The kinds the device was sent, in the order sent.
fn kinds_sent(s: &Setup) -> Vec<String> {
    s.service
        .bodies()
        .iter()
        .map(|body| s.device.decrypt(body)["kind"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn failed_blocked_and_finished_runs_each_send_their_kind_and_started_waits_until_switched_on() {
    let s = setup("dispatch-kinds", false);
    let projects = provider(&s);
    for run in ["run-1", "run-2", "run-3", "run-4", "run-5", "run-6"] {
        seed_run(&s, run, &format!("f-{run}"));
    }
    let mut watch = orq_remote::web_push::RunWatch::new(&s.roots);

    event(
        &s,
        "run-1",
        "run.failed",
        json!({"reason": "the gate failed"}),
    );
    assert_eq!(look(&projects, &mut watch), vec![1]);
    event(&s, "run-2", "run.blocked", json!({"reason": "a conflict"}));
    assert_eq!(look(&projects, &mut watch), vec![1]);
    // Merged locally: the merge, then the run's end.
    event(
        &s,
        "run-3",
        "merge.completed",
        json!({"head_sha": "b".repeat(40), "target_branch": "main"}),
    );
    event(&s, "run-3", "run.completed", json!({"ending": "merged"}));
    assert_eq!(look(&projects, &mut watch), vec![1]);
    // Completed without a forge: pushed, no review.
    event(&s, "run-4", "run.completed", json!({"ending": "pushed"}));
    assert_eq!(look(&projects, &mut watch), vec![1]);
    assert_eq!(kinds_sent(&s), ["failed", "failed", "finished", "finished"]);

    event(&s, "run-5", "run.launched", json!({}));
    assert_eq!(
        look(&projects, &mut watch),
        vec![0],
        "started is off by default"
    );
    orq_core::store::Store::open(&s.roots.database())
        .unwrap()
        .set_push_kind("started", true)
        .unwrap();
    event(&s, "run-6", "run.launched", json!({}));
    assert_eq!(look(&projects, &mut watch), vec![1]);
    assert_eq!(kinds_sent(&s).last().map(String::as_str), Some("started"));
    // Stage events and an ending that is no one's news send nothing.
    event(&s, "run-6", "stage.started", json!({}));
    event(&s, "run-6", "run.completed", json!({"ending": "paused"}));
    assert!(look(&projects, &mut watch).is_empty());
}
