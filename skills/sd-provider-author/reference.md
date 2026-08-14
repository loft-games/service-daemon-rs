# `#[provider]` reference

Complete surface of the `#[provider]` macro and its runtime semantics. All forms
and attributes below are the public macro syntax.

## 1. Provider forms

### Value (default constant)

```rust
use service_daemon::provider;

#[provider(8080)]
pub struct Port(pub i32);

#[provider("mysql://localhost")] // string literal auto-expands to .to_owned()
pub struct DbUrl(pub String);
```

The macro generates a `Default` impl from the value. Inject as `Arc<Port>`.

### Env-backed value

```rust
// String field: env var used directly.
#[provider("localhost:5432", env = "DATABASE_HOST")]
pub struct DatabaseHost(pub String);

// Non-String field: env var auto-parsed via `.parse()`.
#[provider(8080, env = "PORT")]
pub struct Port(pub i32);

// Env-only, no default: initialization FAILS if the env var is missing.
#[provider(env = "API_KEY")]
pub struct ApiKey(pub String);
```

### Template providers

Template names recognized by the macro: `Notify`, `Event`, `Queue`, `BQueue`,
`BroadcastQueue`, `Listen`, `UnixListen`, `UnixConnect`, `NamedPipeListen`,
`NamedPipeConnect`, `LocalIpcListen`, `LocalIpcConnect`. The macro replaces the
struct body with generated code.

```rust
#[provider(Notify)]            // signal source (no payload)
pub struct MySignal;

#[provider(Event)]             // alias family for signal sources
pub struct MyEvent;

#[provider(Queue(String))]     // broadcast queue carrying String payloads
pub struct TaskQueue;

#[provider(Queue(ComplexJob), capacity = 500)] // bounded capacity (must be > 0)
pub struct JobQueue;

#[provider(BQueue(i32))]       // BQueue / BroadcastQueue aliases
pub struct Numbers;

#[provider(Listen("0.0.0.0:8080"))]                 // TCP listener
pub struct ApiListener;

#[provider(Listen("0.0.0.0:8080"), env = "LISTEN_ADDR")] // env overrides addr
pub struct ConfigurableListener;

#[provider(UnixListen("/run/myapp/sock"))]
pub struct ControlSocket;

const PEER_SOCK_PATH: &str = "/run/peer/sock";

#[provider(UnixConnect(PEER_SOCK_PATH), env = "PEER_SOCK", eager = true)]
pub struct PeerSocket;

#[provider(NamedPipeListen(r"\\.\pipe\myapp-api"))]
pub struct WindowsControlPipe;

#[provider(NamedPipeConnect(r"\\.\pipe\peer-api"), env = "PEER_PIPE", eager = true)]
pub struct WindowsPeerPipe;

#[provider(LocalIpcListen("myapp-api"))]
pub struct LocalControlIpc;

#[provider(LocalIpcConnect("peer-api"), env = "PEER_IPC", eager = true)]
pub struct LocalPeerIpc;
```

Syntax rules (enforced by the parser):

- `capacity = N` is only valid on `Queue`/`BQueue` templates and must be `> 0`.
- `Listen`/`UnixListen`/`UnixConnect`/`NamedPipeListen`/`NamedPipeConnect` take
  a string literal or a path to a `const/static &'static str` address/path.
  `LocalIpcListen`/`LocalIpcConnect` take a string literal or `const/static
  &'static str` logical name containing only ASCII letters, digits, `.`, `_`, and
  `-`; `Queue`/`BQueue` take a **type**.
- Dynamic string expressions such as `format!(...)` or function calls are outside
  the public contract for template paths and env names.
- Named attributes (`env`, `capacity`, `eager`) go **outside** the template
  parentheses: `#[provider(Listen("addr"), env = "VAR")]`, never
  `#[provider(Listen("addr", env = "VAR"))]` (that is a compile error).

### Struct provider (composed from other providers)

```rust
use std::sync::Arc;

// Arc<_> fields are resolved as dependencies; non-Arc fields must impl `Default`.
#[provider]
pub struct AppConfig {
    pub port: Arc<Port>,
    pub db_url: Arc<DbUrl>,
}
```

### Async function provider (fallible construction)

```rust
use service_daemon::{provider, ProviderError};
use std::sync::Arc;

#[provider]
pub async fn db_pool(url: Arc<DbUrl>) -> Result<DatabasePool, ProviderError> {
    DatabasePool::connect(&url)
        .await
        .map_err(|e| ProviderError::Retryable(format!("DB connection failed: {e}")))
}
```

## 2. Attributes

| Attribute | Applies to | Meaning |
| :--- | :--- | :--- |
| `env = "VAR"` | value forms | Override/source the value from an env var. Non-String parsed via `.parse()`. |
| `capacity = N` | `Queue`/`BQueue` | Bounded queue capacity, `N > 0`. |
| `eager = true` | any | Initialize at startup instead of lazily (see §4). |

## 3. `ProviderError` model

Fallible providers return `Result<T, ProviderError>`. Return the plain `T`; the
framework wraps it in `Arc<T>`. `ProviderError` is `#[non_exhaustive]` with:

- `ProviderError::Fatal(String)` — unrecoverable. The framework maps it to an
  internal `Fatal` provider-init error and requests **daemon shutdown** at the
  provider-init boundary. No retry.
- `ProviderError::Retryable(String)` — transient. The framework retries with
  backoff until the provider-init timeout, then maps the terminal result to a
  `Timeout` provider-init error.

Classify by whether a retry can plausibly succeed. `Retryable` on permanently
broken config only delays the inevitable timeout; `Fatal` on a not-yet-ready
upstream prevents a recovery that would have worked.

## 4. Lazy vs eager initialization

- **Lazy is the default** for every provider (including `Listen`): it initializes
  the first time a service, trigger, dependency, or helper resolves it.
- `#[provider(..., eager = true)]` initializes during daemon startup. Eager applies
  only to providers **reachable** from the selected services and their dependency
  graph (unreferenced providers are not eagerly built).
- An eager provider that panics during init is converted into a fatal provider-init
  error (it does not unwind the process).

## 5. Retry/timeout engine (what the framework does)

For `Retryable` failures the framework loops: attempt → on `Retryable`, wait
`min(backoff_delay, remaining_time)`, increase backoff, retry — until
`RestartPolicy::provider_init_timeout` elapses. `provider_init_timeout` **defaults
to `wave_spawn_timeout`**; raise it on the daemon's `RestartPolicy` if `Retryable`
providers need a longer window. Cancellation during init or backoff yields a
distinct cancelled outcome (not fatal).

## 6. DI traits (auto-generated — do not hand-implement)

The macro generates three capabilities together:

- `Provided` — resolve a read-only `Arc<T>` snapshot.
- `ManagedProvided` — resolve `Arc<RwLock<T>>` / `Arc<Mutex<T>>` for mutable
  managed state.
- `WatchableProvided` — enables `Watch(T)` triggers (change notification).

Injection sites just declare the type they want (`Arc<T>`, `Arc<RwLock<T>>`,
etc.); the matching trait must exist, which it does for any `#[provider]` type.
Hand-writing these for the same type causes duplicate-impl compile errors.
