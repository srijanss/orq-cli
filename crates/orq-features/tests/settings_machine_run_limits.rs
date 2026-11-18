//! `[runs]` in the user's settings: how many runs may be active on the machine at once, across
//! every project, and how much free disk a run needs to start. Absent or empty is no limit; any
//! other value that is not a whole number of at least 1 is refused, naming the file and the key.

use std::fs;
use std::path::PathBuf;

use orq_features::{MachineRunLimits, Settings};

fn path(label: &str, text: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-machine-limits-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    fs::write(&path, text).unwrap();
    path
}

fn limits(label: &str, text: &str) -> Result<MachineRunLimits, String> {
    Settings::load(&path(label, text)).unwrap().machine_run_limits()
}

#[test]
fn absent_or_empty_is_no_limit_and_whole_numbers_are_read() {
    assert_eq!(limits("absent", "[roles]\nimplementer = \"opus\"\n").unwrap(), MachineRunLimits::default());
    assert_eq!(limits("empty", "[runs]\nmax_active = \"\"\nmin_free_disk_gb = \"\"\n").unwrap(), MachineRunLimits::default());
    assert_eq!(
        limits("set", "[runs]\nmax_active = 2\nmin_free_disk_gb = 15\n").unwrap(),
        MachineRunLimits { max_active: Some(2), min_free_disk_gb: Some(15) },
    );
}

#[test]
fn zero_negative_text_or_fractional_values_are_refused_naming_the_file_and_the_key() {
    for (tag, body, key, shown) in [
        ("zero", "max_active = 0", "max_active", "0"),
        ("negative", "max_active = -1", "max_active", "-1"),
        ("text", "max_active = \"two\"", "max_active", "\"two\""),
        ("disk-zero", "min_free_disk_gb = 0", "min_free_disk_gb", "0"),
        ("disk-float", "min_free_disk_gb = 1.5", "min_free_disk_gb", "1.5"),
    ] {
        let path = path(tag, &format!("[runs]\n{body}\n"));
        let error = Settings::load(&path).unwrap().machine_run_limits().unwrap_err();
        let expected = format!("{}: [runs] {key} must be empty or a whole number of at least 1, got {shown}", path.display());
        assert_eq!(error, expected, "{tag}");
    }
    let path = path("not-a-table", "runs = 3\n");
    let error = Settings::load(&path).unwrap().machine_run_limits().unwrap_err();
    assert_eq!(error, format!("{}: [runs] must be a table", path.display()));
}
