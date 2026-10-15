use std::sync::{Arc, Barrier};

use orq_features::{Settings, SettingsError};

// Alone in its binary: it changes the process working directory.
#[test]
fn a_bare_relative_path_is_serialised_like_any_other() {
    let dir = std::env::temp_dir().join(format!("orq-bare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_current_dir(&dir).unwrap();
    std::fs::write("settings.toml", "[implementer]\nharness = \"base\"\nprofile = \"base\"\n").unwrap();
    let writers = 6;
    let barrier = Arc::new(Barrier::new(writers));

    let handles: Vec<_> = (0..writers)
        .map(|n| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut settings = Settings::load(std::path::Path::new("settings.toml")).unwrap();
                settings.set("implementer", "profile", &format!("writer-{n}"));
                barrier.wait();
                settings.save()
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1, "{results:?}");
    assert!(results
        .iter()
        .filter_map(|r| r.as_ref().err())
        .all(|e| matches!(e, SettingsError::Conflict { .. })));
}
