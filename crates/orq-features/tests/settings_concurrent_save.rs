use std::sync::{Arc, Barrier};

use orq_features::{Settings, SettingsError};

#[test]
fn of_several_writers_from_one_baseline_exactly_one_wins() {
    for round in 0..20 {
        let dir = std::env::temp_dir().join(format!("orq-conc-{}-{round}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        std::fs::write(&path, "[implementer]\nharness = \"base\"\nprofile = \"base\"\n").unwrap();
        let writers = 6;
        let barrier = Arc::new(Barrier::new(writers));

        let handles: Vec<_> = (0..writers)
            .map(|n| {
                let (path, barrier) = (path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let mut settings = Settings::load(&path).unwrap();
                    settings.set("implementer", "profile", &format!("writer-{n}"));
                    barrier.wait();
                    settings.save()
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let wins = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(wins, 1, "round {round}: {results:?}");
        assert!(results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| matches!(e, SettingsError::Conflict { .. })));
    }
}
