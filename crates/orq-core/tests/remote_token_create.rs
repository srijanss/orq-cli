//! Creating the remote token on first use: published whole or not at all.

use orq_core::remote_inbox::RemoteConfig;
use orq_core::storage::StorageRoots;
use std::fs;
use std::path::PathBuf;

fn fresh_roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-token-create-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") }
}

#[test]
fn simultaneous_first_starts_all_get_the_same_complete_token_and_leave_nothing_else_behind() {
    for round in 0..20 {
        let roots = std::sync::Arc::new(fresh_roots(&format!("race-{round}")));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let (roots, barrier) = (roots.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    RemoteConfig::load_or_create(&roots)
                })
            })
            .collect();
        let tokens: Vec<String> = handles.into_iter().map(|h| h.join().unwrap().expect("every start sees a complete token").token).collect();
        assert!(tokens.iter().all(|t| t == &tokens[0] && t.len() >= 32), "round {round}: {tokens:?}");
        let names: Vec<PathBuf> = fs::read_dir(&roots.config).unwrap().map(|e| PathBuf::from(e.unwrap().file_name())).collect();
        assert_eq!(names, [PathBuf::from("remote-token")], "round {round}: no temporary files remain");
    }
}
