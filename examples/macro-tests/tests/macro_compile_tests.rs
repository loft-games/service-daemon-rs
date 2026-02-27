//! Compile-time verification tests for `#[service]`, `#[trigger]`, and `#[provider]` macros.
//!
//! Uses `trybuild` to assert that:
//! - **pass/**: Valid macro usage compiles successfully.
//! - **fail/**: Invalid macro usage produces the expected compile error message.
//!
//! # Adding new test cases
//!
//! 1. Create a `.rs` file in `tests/pass/` (should compile) or `tests/fail/` (should fail).
//! 2. For `fail/` tests, run `cargo test` once to generate the `.stderr` file,
//!    then review and commit the snapshot.

#[test]
fn test_macro_pass_cases() {
    let t = trybuild::TestCases::new();
    t.pass("tests/pass/*.rs");
}

#[test]
fn test_macro_fail_cases() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/fail/*.rs");
}
