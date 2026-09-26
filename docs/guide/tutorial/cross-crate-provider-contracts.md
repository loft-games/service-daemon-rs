# Cross-Crate Provider Contracts

> [!NOTE]
> This is an advanced provider guide, not part of the beginner quick-start path.

Use this pattern when a shared crate owns the type injected by services, but the
final application owns how that value is constructed. For example, a shared
adapter crate can define a connection profile and services that consume it,
while each product application supplies its own connector implementation.

For a single application where the type and its provider live together, the
ordinary [`#[provider]` function](../provider-best-practices.md#2-prefer-provider-async-fn-for-custom-resources)
is simpler. A provider contract is useful when the injectable type must remain
the same across crate boundaries and the application chooses one or more
implementations.

## Crate Layout

The usual split is:

```text
shared-crate/
  contract type + services that inject it

application-crate/
  deployment-specific #[provider_impl] candidates
  daemon binary that links those candidates
```

The shared crate does not need to know which application implementation will be
selected. The service's dependency remains `Arc<ProjectConnectionProfile>` in
every application.

## 1. Define the Contract in the Shared Crate

Mark the shared output struct with `#[provider_contract]`. The macro generates
the provider capabilities for this type, including the compile-time
`ProviderContract` marker required by candidate implementations.

```rust,ignore
use service_daemon::{done, provider_contract, service};
use std::sync::Arc;

#[derive(Clone)]
#[provider_contract]
pub struct ProjectConnectionProfile {
    endpoint: String,
}

impl ProjectConnectionProfile {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

#[service]
pub async fn connection_service(
    profile: Arc<ProjectConnectionProfile>,
) -> anyhow::Result<()> {
    let _endpoint = profile.endpoint();
    done();
    Ok(())
}
```

Keep constructors and fields as restrictive as the shared API requires. The
application can construct the contract through its public API without exposing
implementation details to consuming services. Contract structs must be
concrete (generic contract structs are not supported).

## 2. Register Application-Owned Candidates

In the application crate, define a `#[provider_impl]` function for each
construction strategy. A candidate returns the contract value itself, or
`Result<ContractType, ProviderError>` when it can fail. Do not return `Arc<T>`;
the framework wraps the selected value for injection.

```rust,ignore
use project_shared::ProjectConnectionProfile;
use service_daemon::{ProviderError, provider_impl};

#[provider_impl(priority = 80)]
async fn primary_profile() -> Result<ProjectConnectionProfile, ProviderError> {
    // Replace this with the application's real configuration or connection
    // check. Unavailable means this candidate does not apply in this deployment.
    Err(ProviderError::Unavailable(
        "primary endpoint is not configured".to_owned(),
    ))
}

#[provider_impl(priority = 10)]
async fn fallback_profile() -> ProjectConnectionProfile {
    ProjectConnectionProfile::new("fallback.internal:443")
}
```

`#[provider_impl]` belongs on a free function in the application crate. Its
output type must be marked with `#[provider_contract]`; the compile-time bound
prevents accidentally registering an ordinary, unrelated type. The candidate
function may be private. The shared contract type remains the service's DI key:

```rust,ignore
async fn another_service(profile: Arc<ProjectConnectionProfile>) {
    // Same injected type regardless of which candidate succeeds.
}
```

Candidate functions can also declare provider dependencies as `Arc<T>`
parameters. Dependencies are resolved only when that candidate is attempted;
lower-priority candidates do not initialize their dependencies after an earlier
candidate succeeds. All candidate dependency edges are still checked for
startup cycles.

For example, the application can build the shared profile from its own MQTT
configuration and connector. `MqttConfig` is an application-owned ordinary
provider; the shared crate only knows the `AdapterConnector` interface:

```rust,ignore
use std::sync::Arc;
use project_shared::InstalledConnectionProfile;
use service_daemon::{ProviderError, provider, provider_impl};

#[provider("mqtt://localhost:1883", env = "MQTT_URL")]
pub struct MqttConfig(pub String);

#[provider_impl(priority = 80)]
async fn u200t_connection_profile_provider(
    mqtt: Arc<MqttConfig>,
) -> Result<InstalledConnectionProfile, ProviderError> {
    let connector = U200tMqttConnector::new(mqtt.0.clone());
    Ok(InstalledConnectionProfile::new(Arc::new(connector)))
}
```

Here `InstalledConnectionProfile::new` accepts the shared trait-object type,
such as `Arc<dyn AdapterConnector>`, and the application adapts its concrete
connector to that interface. A real provider can return
`ProviderError::Unavailable` when that deployment does not use MQTT,
`Retryable` while a transient dependency is unavailable, or `Fatal` for
permanent configuration errors.

## 3. Make the Candidate Crate Part of the Final Target

The macros register candidates at link time. A module in the binary must be
included in its module tree:

```rust,ignore
mod providers;
```

If candidates live in a separate application library crate, every final binary
or integration-test target that needs them must depend on and link that crate.
When the target has no other use for the library, add an underscore import:

```rust,ignore
use project_provider_impl as _;
```

Listing the crate in `Cargo.toml` alone does not ensure an otherwise-unused
library is linked. Integration tests are separate final targets, so add the
import to each test target that relies on those registrations as well.

## 4. Choose Candidate Priority and Failure Behavior

Candidates are attempted by descending `priority`. The default is `50`; equal
priorities have a stable order based on module path and function name. Choose
priorities to express the intended fallback order, not as a runtime tuning knob.

| Result | Contract behavior |
| :--- | :--- |
| Return `T` / `Ok(T)` | Select this candidate and cache its value for the daemon. |
| `ProviderError::Unavailable` | This candidate does not apply; try the next candidate. |
| `ProviderError::Retryable` | Retry this candidate until its provider-init timeout; then try the next candidate. |
| `ProviderError::Fatal` | Stop resolving the contract; do not try lower-priority candidates. |

`Unavailable` is specific to candidate selection. An ordinary `#[provider]`
has no candidate chain, so returning `Unavailable` from it is fatal. If no
candidate is registered, or all candidates are exhausted without producing a
value, contract initialization fails fatally and emits an error-level log.

## 5. Choose Lazy or Eager Initialization

Contracts are lazy by default and initialize when first resolved. Use
`#[provider_contract(eager = true)]` only when the contract must initialize
during daemon startup. As with other eager providers, only contracts reachable
from selected services and their dependency graph are initialized. Eager mode
changes initialization timing, not which crate owns the contract or candidates.

## Run the Complete Example

The runnable example demonstrates a shared crate that defines the contract and
consumer service, plus an application library whose first candidate is
unavailable and whose second candidate supplies the value. Its binary and
integration test explicitly link the candidate library.

```bash
cargo run -p example-provider-contract
cargo test -p example-provider-contract
```

See the [example crate layout](../../../examples/provider-contract/README.md)
and the [Provider Strategy reference](../provider-best-practices.md#4-cross-crate-provider-contracts)
for the complete macro contract, retry details, and candidate rules.

---

[Back to Custom Providers](./custom-providers.md) | [Provider Strategy Reference](../provider-best-practices.md)
