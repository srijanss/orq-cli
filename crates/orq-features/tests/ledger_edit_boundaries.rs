use orq_core::FeatureStatus;
use orq_features::{set_entry_status, EditError};

fn edit(text: &str, name: &str) -> Result<String, EditError> {
    set_entry_status(text, name, FeatureStatus::Pending)
}

#[test]
fn shapes_that_are_not_a_feature_list_are_malformed() {
    let cases = [
        ("{ nope", "syntax error"),
        ("{\"featureName\":\"a\",\"status\":\"draft\"}", "object root"),
        ("[42]", "non-object entry"),
        ("[{\"featureName\":\"a\"}]", "target without status"),
        ("[{\"featureName\":\"a\",\"status\":7}]", "target with non-string status"),
    ];
    for (text, label) in cases {
        assert_eq!(edit(text, "a"), Err(EditError::Malformed), "{label}");
    }
}

#[test]
fn a_name_that_is_absent_is_unknown_not_malformed() {
    assert_eq!(edit("[]", "a"), Err(EditError::UnknownFeature));
    assert_eq!(
        edit("[{\"featureName\":\"b\",\"status\":\"draft\"}]", "a"),
        Err(EditError::UnknownFeature)
    );
}

#[test]
fn escapes_unicode_and_delimiters_in_other_fields_survive_byte_for_byte() {
    let text = "[\n  {\n    \"description\": \"caf\u{e9} \\\"}]{,\\\\ \u{1f600}\",\n    \"feature\\u004eame\": \"a\\u0062\",\n    \"st\\u0061tus\": \"draft\"\n  }\n]\n";

    let edited = edit(text, "ab").unwrap();

    assert_eq!(edited, text.replacen("\"draft\"", "\"pending\"", 1));
}
