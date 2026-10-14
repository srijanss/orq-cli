#![cfg(unix)]

use std::fs;
use std::sync::Arc;
use std::thread;

use orq_features::write_ledger_atomic;

#[test]
fn concurrent_writers_never_collide_and_readers_see_whole_payloads() {
    let dir = std::env::temp_dir().join(format!("orq-atomic-concurrent-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = Arc::new(dir.join(".tdd-features.json"));
    fs::write(&*path, "A".repeat(4096)).unwrap();

    let payloads: Vec<String> = (0..8u8).map(|n| char::from(b'a' + n).to_string().repeat(4096)).collect();
    let writers: Vec<_> = payloads
        .iter()
        .cloned()
        .map(|payload| {
            let path = Arc::clone(&path);
            thread::spawn(move || {
                for _ in 0..50 {
                    write_ledger_atomic(&path, &payload).unwrap();
                }
            })
        })
        .collect();
    let reader = {
        let path = Arc::clone(&path);
        thread::spawn(move || {
            for _ in 0..200 {
                let seen = fs::read_to_string(&*path).unwrap();
                let first = seen.chars().next().unwrap();
                assert_eq!(seen.len(), 4096);
                assert!(seen.chars().all(|c| c == first), "mixed payload");
            }
        })
    };

    for writer in writers {
        writer.join().unwrap();
    }
    reader.join().unwrap();
    let leftovers: Vec<_> = fs::read_dir(&*dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(leftovers.len(), 1, "temp files left behind: {leftovers:?}");
}
