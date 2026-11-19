//! A test that needs variables in its own process environment (what the programs it starts inherit)
//! runs again alone in a child process given them, so the tests running beside it never see them.

use std::ffi::OsStr;

const NAME: &str = "ORQ_OWN_PROCESS_ENV_PROBE";

#[test]
fn a_test_run_in_its_own_process_sees_the_variables_and_the_caller_never_gets_them() {
    let value = OsStr::new("only-in-the-child");
    if !orq_test_support::in_own_process_with_env(
        "a_test_run_in_its_own_process_sees_the_variables_and_the_caller_never_gets_them",
        &[(NAME, value)],
    ) {
        assert_eq!(std::env::var_os(NAME).as_deref(), Some(value));
        return;
    }
    assert_eq!(std::env::var_os(NAME), None, "the caller's own environment was changed");
}
