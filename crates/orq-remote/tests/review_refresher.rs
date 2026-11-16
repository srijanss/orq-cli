//! `orq serve` keeps open provider reviews fresh: a round looks again at each open review last looked
//! at ten minutes ago or more, a review opened on the page is looked at when its last look is two
//! minutes old, one look at a time; merged and closed reviews are not looked at again.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orq_core::remote_inbox::RemoteConfig;
use orq_core::review_lifecycle::{Observation, ObservationError, ProviderState, ReviewLifecycle};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_remote::review_refresh::{ReviewObserver, ReviewRefresher};
use orq_remote::{Server, ServerOptions, ServerState};

const TOKEN: &str = "refresher-test-token-0123456789abcdef";

fn head(n: u8) -> String {
    format!("{n}").repeat(40)
}

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-refresher-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// `run` with a report at head `n` and a provider review in `state`, last looked at `age` seconds ago.
fn review(roots: &StorageRoots, run: &str, n: u8, state: ProviderState, age: i64) {
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, run, "p", "F", &head(n), br#"{"status":"current","risks":[]}"#).unwrap();
    let seen = ReviewLifecycle::associate("github", "7", "https://github.example/o/r/pull/7", &head(n)).apply(Ok(Observation { state, head_sha: head(n) }));
    store.save_lifecycle(run, &seen).unwrap();
    age_check(roots, run, age);
}

fn age_check(roots: &StorageRoots, run: &str, age: i64) {
    rusqlite::Connection::open(roots.database()).unwrap().execute("UPDATE review_lifecycles SET last_checked = ?1 WHERE run_id = ?2", rusqlite::params![now() - age, run]).unwrap();
}

/// Records the runs it is asked about and how many looks overlapped; each look takes `delay` and
/// saves what `result` says, as a real look does.
struct Recorder {
    roots: StorageRoots,
    asked: Mutex<Vec<String>>,
    running: Mutex<(usize, usize)>,
    delay: Duration,
    fail: bool,
}

impl Recorder {
    fn new(roots: &StorageRoots, delay: Duration, fail: bool) -> Arc<Recorder> {
        Arc::new(Recorder { roots: roots.clone(), asked: Mutex::new(Vec::new()), running: Mutex::new((0, 0)), delay, fail })
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    fn most_at_once(&self) -> usize {
        self.running.lock().unwrap().1
    }
}

impl ReviewObserver for Recorder {
    fn refresh(&self, run_id: &str) {
        {
            let mut running = self.running.lock().unwrap();
            running.0 += 1;
            running.1 = running.1.max(running.0);
        }
        self.asked.lock().unwrap().push(run_id.to_string());
        std::thread::sleep(self.delay);
        let store = Store::open(&self.roots.database()).unwrap();
        let kept = store.lifecycles_for_run(run_id).unwrap().pop().unwrap();
        let seen = if self.fail { Err(ObservationError::Unavailable("HTTP 502".into())) } else { Ok(Observation { state: kept.state, head_sha: kept.reported_head_sha.clone() }) };
        store.save_lifecycle(run_id, &kept.apply(seen)).unwrap();
        self.running.lock().unwrap().0 -= 1;
    }
}

#[test]
fn a_round_looks_once_at_each_open_review_last_looked_at_ten_minutes_ago_or_more() {
    let roots = roots("round");
    review(&roots, "run-due", 1, ProviderState::Open, 601);
    review(&roots, "run-recent", 2, ProviderState::Open, 300);
    review(&roots, "run-merged", 3, ProviderState::Merged, 3600);
    review(&roots, "run-closed", 4, ProviderState::Closed, 3600);
    let recorder = Recorder::new(&roots, Duration::ZERO, false);
    let refresher = ReviewRefresher::new(roots.clone(), recorder.clone());

    assert_eq!(refresher.round(now()), 1);
    assert_eq!(recorder.asked(), vec!["run-due"]);
    assert_eq!(refresher.round(now()), 0, "the look just made is recent");
    assert_eq!(refresher.round(now() + 601), 2, "both open reviews are due again ten minutes on; merged and closed never are");
}

#[test]
fn a_look_that_fails_leaves_the_review_unverified_and_it_is_looked_at_again_next_round() {
    let roots = roots("failed");
    review(&roots, "run-1", 1, ProviderState::Open, 601);
    let recorder = Recorder::new(&roots, Duration::ZERO, true);
    let refresher = ReviewRefresher::new(roots.clone(), recorder.clone());

    assert_eq!(refresher.round(now()), 1);
    let kept = Store::open(&roots.database()).unwrap().lifecycles_for_run("run-1").unwrap();
    assert!(kept[0].last_error.is_some() && kept[0].state == ProviderState::Open, "{kept:?}");
    assert_eq!(refresher.round(now() + 601), 1, "retried on the next round");
}

#[test]
fn an_opened_review_is_looked_at_when_its_last_look_is_two_minutes_old() {
    let roots = roots("opened");
    review(&roots, "run-old", 1, ProviderState::Open, 121);
    review(&roots, "run-fresh", 2, ProviderState::Open, 60);
    review(&roots, "run-merged", 3, ProviderState::Merged, 3600);
    let recorder = Recorder::new(&roots, Duration::ZERO, false);
    let refresher = ReviewRefresher::new(roots.clone(), recorder.clone());

    assert!(refresher.opened("run-old", now()));
    assert!(!refresher.opened("run-old", now()), "looked at just now");
    assert!(!refresher.opened("run-fresh", now()));
    assert!(!refresher.opened("run-merged", now()));
    assert!(!refresher.opened("run-unknown", now()));
    assert!(!refresher.opened("../escape", now()));
    assert_eq!(recorder.asked(), vec!["run-old"]);
}

#[test]
fn looks_are_made_one_at_a_time_and_a_review_looked_at_meanwhile_is_not_looked_at_twice() {
    let roots = roots("serial");
    review(&roots, "run-a", 1, ProviderState::Open, 601);
    review(&roots, "run-b", 2, ProviderState::Open, 601);
    let recorder = Recorder::new(&roots, Duration::from_millis(200), false);
    let refresher = ReviewRefresher::new(roots.clone(), recorder.clone());

    let round = { let r = refresher.clone(); std::thread::spawn(move || r.round(now())) };
    std::thread::sleep(Duration::from_millis(50));
    let opened: Vec<_> = ["run-a", "run-b"].into_iter().map(|run| { let r = refresher.clone(); std::thread::spawn(move || r.opened(run, now())) }).collect();
    let round = round.join().unwrap();
    let opened: usize = opened.into_iter().map(|t| usize::from(t.join().unwrap())).sum();

    assert_eq!(recorder.most_at_once(), 1);
    assert_eq!(round + opened, 2, "{:?}", recorder.asked());
}

fn get(addr: std::net::SocketAddr, path: &str, token: Option<&str>) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{auth}\r\n").unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    reply
}

#[test]
fn the_server_looks_at_a_review_a_signed_in_page_opens_and_runs_rounds() {
    let roots = roots("server");
    review(&roots, "run-opened", 1, ProviderState::Open, 300);
    let recorder = Recorder::new(&roots, Duration::ZERO, false);
    let refresher = ReviewRefresher::new(roots.clone(), recorder.clone());
    let state = Arc::new(ServerState { roots: roots.clone(), config: RemoteConfig { token: TOKEN.into() }, project_id: "p".into(), project_name: "p".into() });
    let server = Server::start_with(TcpListener::bind("127.0.0.1:0").unwrap(), state, ServerOptions { reviews: Some(refresher), ..Default::default() }).unwrap();

    assert!(get(server.addr(), "/api/reviews/run-opened", None).starts_with("HTTP/1.1 401"));
    std::thread::sleep(Duration::from_millis(300));
    assert!(recorder.asked().is_empty(), "a refused request looks at nothing");

    assert!(get(server.addr(), "/api/reviews/run-opened", Some(TOKEN)).starts_with("HTTP/1.1 200"));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while recorder.asked().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(recorder.asked(), vec!["run-opened"]);

    review(&roots, "run-due", 2, ProviderState::Open, 601);
    drop(server);
    let rounds = orq_remote::review_refresh::start_rounds(ReviewRefresher::new(roots.clone(), recorder.clone()), Duration::from_millis(50));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !recorder.asked().contains(&"run-due".to_string()) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(rounds);
    assert_eq!(recorder.asked(), vec!["run-opened", "run-due"]);
}

#[test]
fn a_run_merged_locally_is_not_looked_at_though_an_earlier_report_kept_an_open_review() {
    let roots = roots("merged-here");
    review(&roots, "run-1", 1, ProviderState::Open, 3600);
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-1", "p", "F", &head(2), br#"{"status":"current","risks":[]}"#).unwrap();
    store.save_lifecycle("run-1", &ReviewLifecycle::merged_locally(&head(2), &head(9))).unwrap();
    let recorder = Recorder::new(&roots, Duration::ZERO, false);
    let refresher = ReviewRefresher::new(roots.clone(), recorder.clone());

    assert_eq!(refresher.round(now() + 3600), 0);
    assert!(!refresher.opened("run-1", now() + 3600));
    assert!(recorder.asked().is_empty());
}

#[test]
fn stopping_the_rounds_waits_only_for_the_look_under_way_and_starts_no_other() {
    let roots = roots("stop");
    for (n, run) in ["run-a", "run-b", "run-c"].into_iter().enumerate() {
        review(&roots, run, n as u8 + 1, ProviderState::Open, 601);
    }
    let recorder = Recorder::new(&roots, Duration::from_millis(400), false);
    let rounds = orq_remote::review_refresh::start_rounds(ReviewRefresher::new(roots.clone(), recorder.clone()), Duration::from_secs(60));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while recorder.asked().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    let stopping = std::time::Instant::now();
    drop(rounds);
    assert!(stopping.elapsed() < Duration::from_millis(700), "{:?}", stopping.elapsed());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(recorder.asked().len(), 1, "{:?}", recorder.asked());
}
