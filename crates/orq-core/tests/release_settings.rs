//! The `[release]` table of a project's `.orq/run.toml`: whether a merge is released, which file and key
//! hold the version, and the argv lists run after the bump. A bad value is an error naming the key.

use orq_core::project_settings::{version_in, with_version, ReleaseOnMerge, ReleaseSettings};

#[test]
fn a_version_is_read_and_replaced_in_a_toml_key_or_a_json_field_keeping_the_rest_of_the_file() {
    let toml = "# top\n[workspace.package]\nversion = \"0.1.2\" # the release\nedition = \"2021\"\n\n[dependencies]\nx = { version = \"0.1.2\" }\n";
    assert_eq!(version_in("Cargo.toml", toml, "workspace.package.version").unwrap(), "0.1.2");
    let bumped = with_version("Cargo.toml", toml, "workspace.package.version", "0.1.3").unwrap();
    assert_eq!(bumped, toml.replacen("version = \"0.1.2\" # the release", "version = \"0.1.3\" # the release", 1));
    let json = "{\n  \"name\": \"x\",\n  \"version\": \"1.0.0\",\n  \"dependencies\": {\"y\": \"1.0.0\"}\n}\n";
    assert_eq!(version_in("package.json", json, "version").unwrap(), "1.0.0");
    assert_eq!(with_version("package.json", json, "version", "1.1.0").unwrap(), json.replace("\"version\": \"1.0.0\"", "\"version\": \"1.1.0\""));
    for (file, text, key) in [("Cargo.toml", toml, "package.version"), ("Cargo.toml", "x = [", "x"), ("Cargo.toml", "[a]\nv = 3\n", "a.v"), ("package.json", json, "name.x"), ("package.json", "{", "version"), ("notes.txt", "version", "version")] {
        assert!(version_in(file, text, key).is_err(), "{file} {key}");
        assert!(with_version(file, text, key, "9.9.9").is_err(), "{file} {key}");
    }
}

#[test]
fn an_auto_release_table_is_read_with_its_version_key_and_commands() {
    let text = "[release]\nrelease_on_merge = \"auto\"\nversion_file = \"package.json\"\nversion_key = \"version\"\ncommands = [[\"cargo\", \"update\"]]\n";
    let settings = ReleaseSettings::parse(text).unwrap().unwrap();
    assert_eq!(settings.release_on_merge, ReleaseOnMerge::Auto);
    assert_eq!(settings.version_file, "package.json");
    assert_eq!(settings.version_key, "version");
    assert_eq!(settings.commands, vec![vec!["cargo".to_string(), "update".to_string()]]);
}

#[test]
fn a_release_table_with_bad_values_or_none_is_an_error_naming_the_key_or_nothing() {
    assert_eq!(ReleaseSettings::parse("[after_merge]\n").unwrap(), None);
    let defaults = ReleaseSettings::parse("[release]\n").unwrap().unwrap();
    assert_eq!(defaults.release_on_merge, ReleaseOnMerge::Off);
    assert_eq!((defaults.version_file.as_str(), defaults.version_key.as_str()), ("Cargo.toml", "workspace.package.version"));
    assert!(defaults.commands.is_empty());
    for (text, key) in [
        ("[release]\nrelease_on_merge = \"sometimes\"\n", "release.release_on_merge"),
        ("[release]\nversion_file = 3\n", "release.version_file"),
        ("[release]\nversion_key = \"\"\n", "release.version_key"),
        ("[release]\ncommands = [\"cargo update\"]\n", "release.commands[0]"),
        ("[release]\ncommands = [[]]\n", "release.commands[0]"),
    ] {
        let error = ReleaseSettings::parse(text).unwrap_err();
        assert!(error.contains(key), "{text}: {error}");
    }
}
