//! Compile-time verification tests for `#[service]`, `#[trigger]`, and `#[provider]` macros.
//!
//! Uses `trybuild` to assert valid macro usage and expected compile errors.
//! Cases are grouped by macro surface so filtered commands such as `-- provider`
//! run only the relevant fixtures.
//!
//! # Adding new test cases
//!
//! 1. Create a `.rs` file in `tests/pass/` (should compile) or `tests/fail/` (should fail).
//! 2. For `fail/` tests, run `cargo test` once to generate the `.stderr` file,
//!    then review and commit the snapshot.

const SERVICE_PASS_CASES: &[&str] = &[
    "tests/pass/01_basic_service.rs",
    "tests/pass/02_service_with_priority_and_tags.rs",
    "tests/pass/05_allow_sync_service.rs",
    "tests/pass/17_service_visibility_super.rs",
    "tests/pass/19_service_visibility_in_self_path.rs",
    "tests/pass/20_service_visibility_in_super_path.rs",
    "tests/pass/27_service_wrapper_path_visibility.rs",
    "tests/pass/28_service_handle_macro_path_visibility.rs",
    "tests/pass/29_service_auto_start_false.rs",
];

const SERVICE_FAIL_CASES: &[&str] = &[
    "tests/fail/01_service_bare_param.rs",
    "tests/fail/02_service_rejects_payload.rs",
    "tests/fail/03_service_unknown_attr.rs",
    "tests/fail/10_service_explicit_payload_attr.rs",
    "tests/fail/11_only_provided_cannot_inject_managed.rs",
    "tests/fail/13_service_private_not_visible_to_sibling.rs",
    "tests/fail/14_service_invalid_scheduling.rs",
    "tests/fail/15_service_auto_scheduling.rs",
    "tests/fail/33_service_handle_rejects_generic_path.rs",
];

const TRIGGER_PASS_CASES: &[&str] = &["tests/pass/18_trigger_visibility_super.rs"];

const TRIGGER_FAIL_CASES: &[&str] = &[
    "tests/fail/08_trigger_multiple_payloads.rs",
    "tests/fail/09_trigger_payload_plus_dependency_hint.rs",
    "tests/fail/12_non_watchable_provider_cannot_watch.rs",
    "tests/fail/16_trigger_control_scheduling.rs",
];

const PROVIDER_PASS_CASES: &[&str] = &[
    "tests/pass/03_provider_with_defaults.rs",
    "tests/pass/04_provider_struct_with_deps.rs",
    "tests/pass/07_provider_rwlock_mutex_injection.rs",
    "tests/pass/08_provider_queue_with_capacity.rs",
    "tests/pass/09_provider_env_syntax.rs",
    "tests/pass/10_provider_env_int_type.rs",
    "tests/pass/11_provider_env_only_string.rs",
    "tests/pass/12_provider_fn_with_deps.rs",
    "tests/pass/13_provider_queue_default_type.rs",
    "tests/pass/14_provider_sync_fn.rs",
    "tests/pass/15_provider_queue_with_bqueue_alias.rs",
    "tests/pass/17_provider_fallible_helpers.rs",
    "tests/pass/22_provider_return_type_contracts.rs",
    "tests/pass/24_provider_default_expression_boundary.rs",
    "tests/pass/25_provider_explicit_key_forms.rs",
    "tests/pass/26_provider_local_ipc_templates.rs",
];

const PROVIDER_FAIL_CASES: &[&str] = &[
    "tests/fail/04_provider_on_enum.rs",
    "tests/fail/05_provider_fn_no_return.rs",
    "tests/fail/05_provider_unknown_template.rs",
    "tests/fail/06_provider_fn_bare_param.rs",
    "tests/fail/07_provider_fn_self_param.rs",
    "tests/fail/17_provider_queue_zero_capacity.rs",
    "tests/fail/18_provider_value_capacity.rs",
    "tests/fail/19_provider_malformed_eager.rs",
    "tests/fail/20_provider_duplicate_env.rs",
    "tests/fail/21_provider_duplicate_capacity.rs",
    "tests/fail/22_provider_duplicate_eager.rs",
    "tests/fail/23_provider_custom_provider_error.rs",
    "tests/fail/24_provider_unsafe_fn.rs",
    "tests/fail/25_provider_result_non_provider_error.rs",
    "tests/fail/26_provider_custom_path_template_not_open.rs",
    "tests/fail/27_provider_template_named_attr_inside_parens.rs",
    "tests/fail/29_provider_local_ipc_capacity_attr.rs",
    "tests/fail/32_provider_local_ipc_invalid_name.rs",
    "tests/fail/33_provider_local_ipc_inner_attr.rs",
];

#[cfg(not(windows))]
const PROVIDER_PLATFORM_FAIL_CASES: &[&str] = &[
    "tests/fail/30_provider_named_pipe_listen_non_windows.rs",
    "tests/fail/31_provider_named_pipe_connect_non_windows.rs",
];

#[cfg(windows)]
const PROVIDER_PLATFORM_FAIL_CASES: &[&str] = &[];

const INTEGRATION_PASS_CASES: &[&str] = &[
    "tests/pass/06_trigger_templates.rs",
    "tests/pass/16_provider_full_capabilities.rs",
    "tests/pass/21_service_scheduling.rs",
    "tests/pass/23_visibility_restricted_edges.rs",
];

fn run_cases(pass_cases: &[&str], fail_cases: &[&str]) {
    let test_cases = trybuild::TestCases::new();

    for pass_case in pass_cases {
        test_cases.pass(pass_case);
    }

    for fail_case in fail_cases {
        test_cases.compile_fail(fail_case);
    }
}

#[test]
fn service_macro_cases() {
    run_cases(SERVICE_PASS_CASES, SERVICE_FAIL_CASES);
}

#[test]
fn trigger_macro_cases() {
    run_cases(TRIGGER_PASS_CASES, TRIGGER_FAIL_CASES);
}

#[test]
fn provider_macro_cases() {
    let mut fail_cases = Vec::from(PROVIDER_FAIL_CASES);
    fail_cases.extend_from_slice(PROVIDER_PLATFORM_FAIL_CASES);
    run_cases(PROVIDER_PASS_CASES, &fail_cases);
}

#[test]
fn integration_macro_cases() {
    run_cases(INTEGRATION_PASS_CASES, &[]);
}
