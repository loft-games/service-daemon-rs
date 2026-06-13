//! Linkme registration smoke test.
//!
//! Guards against the linker silently garbage-collecting `#[service]` and
//! `#[provider]` entries before the daemon can discover them. Has been observed
//! locally on `x86_64-pc-windows-gnu` + rustc 1.95 + linkme 0.3.35 and would
//! make service-discovery-based tests pass with zero discovered services.
//!
//! If this test fails on a platform that previously passed, investigate in
//! order: (1) rustc / LLVM upgrade changing link-section handling,
//! (2) linkme crate version, (3) linker flags (try `-Wl,--no-gc-sections`).
//!
//! The service and provider defined here are intentionally never used by a
//! daemon. The test only asserts that their registry entries survive the link
//! step.

use service_daemon::{
    __private::{PROVIDER_REGISTRY, SERVICE_REGISTRY},
    Registry, provider, service,
};

const SMOKE_TAG: &str = "__linkme_smoke__";

#[derive(Clone, Default)]
#[provider]
pub struct LinkmeSmokeConfig;

#[service(tags = ["__linkme_smoke__"])]
async fn linkme_smoke_service(_config: std::sync::Arc<LinkmeSmokeConfig>) -> anyhow::Result<()> {
    Ok(())
}

#[test]
fn linkme_preserves_registry_entries() {
    let total = SERVICE_REGISTRY.iter().count();
    assert!(
        total > 0,
        "SERVICE_REGISTRY is empty. The linker dropped the linkme section. \
         Check rustc/LLVM/linkme versions; consider `-Wl,--no-gc-sections`."
    );

    let filtered = Registry::builder().with_tag(SMOKE_TAG).build();
    assert!(
        !filtered.is_empty(),
        "Registry could not find the #[service] defined in this test binary. \
         SERVICE_REGISTRY saw {total} entries overall, but tag '{SMOKE_TAG}' \
         matched none. The linker likely dropped test-binary-local entries."
    );

    let provider_found = PROVIDER_REGISTRY
        .iter()
        .any(|entry| entry.name == "LinkmeSmokeConfig");
    assert!(
        provider_found,
        "PROVIDER_REGISTRY could not find the #[provider] defined in this test \
         binary. The linker likely dropped test-binary-local provider entries."
    );
}
