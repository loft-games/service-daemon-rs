# Windows Named Pipe Provider Contract

This note records the design direction for Windows local IPC provider templates
and the boundary they keep after the cross-platform `LocalIpc` facade was added.

## Decision

Use explicit Windows-only provider templates:

```rust
#[provider(NamedPipeListen(r"\\.\pipe\myapp-api"))]
pub struct ApiPipe;

#[provider(NamedPipeConnect(r"\\.\pipe\peer-api"), eager = true)]
pub struct PeerPipe;
```

Do not make `UnixListen` or `UnixConnect` mean named pipes on Windows. Existing
Unix socket templates stay Unix-only with their current `compile_error!` guard
on non-Unix targets.

`LocalIpcListen` / `LocalIpcConnect` now exist as a logical-name facade over
Unix sockets and Windows named pipes. They do not accept raw platform endpoints;
explicit `NamedPipe*` templates remain the low-level Windows API when code needs
native pipe-path control.

## Why Not Windows AF_UNIX

The current Unix templates generate `std::os::unix::net` and
`tokio::net::UnixStream` / `UnixListener` APIs. Those APIs are Unix-specific in
Rust and Tokio. Even if a Windows deployment can use AF_UNIX at the OS level,
that does not make it a drop-in target for the existing generated Rust API.

Named pipes are the Windows local IPC primitive with first-class Tokio support
under `tokio::net::windows::named_pipe`. They have a different lifecycle model,
different connection errors, and different security controls from Unix domain
sockets, so they need explicit template names and Windows-specific tests.

## Generated Server Shape

`NamedPipeListen(Path)` should generate a Windows-only wrapper around a small
framework-owned server factory, not a single reusable connected server handle.

Generated public shape:

```rust
impl ApiPipe {
    pub fn name(&self) -> &str;

    pub async fn accept(
        &self,
    ) -> std::io::Result<service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer>;
}
```

`accept().await?` is the common server path. It must create or reuse a pending
server instance, wait for a client with `NamedPipeServer::connect().await`, and
return the connected `NamedPipeServer`, which implements `AsyncRead` and
`AsyncWrite`.

The implementation should keep at least one server instance available while
handing a connected instance to user code. Tokio's named pipe docs describe this
as necessary to avoid clients intermittently seeing `NotFound` between accepted
connections. A framework wrapper is therefore preferable to exposing raw
`ServerOptions` and asking every service to repeat the instance rollover loop.

Initial server options:

- `ServerOptions::new()`
- `first_pipe_instance(true)` for the first provider-created instance in this
  process, so another server process on the same pipe name fails clearly.
- `reject_remote_clients(true)` even though Tokio disables remote clients by
  default; set it explicitly so the generated security intent remains visible.
- default byte mode unless a later API explicitly introduces message mode.

The provider value should store the pipe name and enough immutable options to
create server instances on demand. It should not expose mutable option builders
through the provider instance.

## Generated Client Shape

`NamedPipeConnect(Path)` should mirror `UnixConnect`: hold the pipe name and open
a fresh client stream per call.

Generated public shape:

```rust
impl PeerPipe {
    pub fn name(&self) -> &str;

    pub async fn connect(
        &self,
    ) -> std::io::Result<service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeClient>;
}
```

Provider initialization should perform one reachability probe and immediately
drop the connected client, matching `UnixConnect` semantics. With `eager = true`,
this blocks the daemon startup wave until the peer pipe is reachable or provider
init retry policy expires.

Each later `connect().await?` opens a fresh independent `NamedPipeClient` using
`ClientOptions::new().open(name)`. The framework should not pool named pipe
clients in the first implementation.

## Error Classification

Server-side `NamedPipeListen` provider init:

| Error | Strategy | Reason |
| :--- | :--- | :--- |
| `PermissionDenied` | Fatal | Usually means access denied, another first instance exists, or policy blocks pipe creation. |
| `InvalidInput` | Fatal | Invalid pipe name or unsupported options. |
| `AddrInUse` | Retryable only if observed from Tokio / Windows mapping | Race with another instance during startup. |
| raw `ERROR_PIPE_BUSY` | Retryable | Instance pressure or concurrent creation race. |
| Other I/O | Fatal by default | Do not hide unknown Windows pipe failures as transient until tests justify it. |

Client-side `NamedPipeConnect` provider init:

| Error | Strategy | Reason |
| :--- | :--- | :--- |
| `NotFound` | Retryable during provider init | Peer has not created the pipe yet. |
| raw `ERROR_PIPE_BUSY` | Retryable | All server instances are busy; Tokio documents retrying this case. |
| `ConnectionRefused` / `ConnectionAborted` | Retryable if observed | Peer startup or immediate close race. |
| `PermissionDenied` | Fatal | Security policy or access rights are wrong. |
| `InvalidInput` | Fatal | Invalid pipe name or unsupported options. |
| Other I/O | Fatal by default | Unknown Windows pipe failures should be visible first. |

Runtime `connect().await` opens one fresh client and returns the raw I/O result.
Callers that expect short listener-replacement windows should retry raw
`ERROR_PIPE_BUSY` at the call site, as the named-pipe example and roundtrip test
do.

The final provider-init mapping should use the same boundary as `Listen`,
`UnixListen`, and `UnixConnect`: retryable errors feed `ProviderError::Retryable`
until `RestartPolicy::provider_init_timeout`, fatal errors become
`ProviderInitError::Fatal`.

## Security Boundary

The first implementation should be local-only:

- explicitly set `reject_remote_clients(true)` on server options;
- do not expose unsafe `SECURITY_ATTRIBUTES` pointers through the macro API;
- do not add ACL or security descriptor management in the first patch;
- document that administrators must still use OS policy to control which local
  users can open the pipe until a reviewed ACL API exists.

Tokio exposes raw security-attribute entry points, but this framework should not
surface those through generated code without a separate security design.

## Parser and Codegen Plan

Add parser support only after this contract is accepted:

1. Add `NamedPipeListen` and `NamedPipeConnect` to provider built-in template
   classification.
2. Parse both template arguments as string literals in template dispatch.
3. Generate targeted `#[cfg(windows)]` code and non-Windows `compile_error!`
   guards, mirroring the current Unix template style.
4. Keep `env = "..."` and `eager = true` behavior aligned with Unix templates.
5. Reject `capacity` with the same warning/diagnostic policy used by other
   non-queue templates.

Do not use arbitrary `Path(...)` provider heads as open named pipe templates.

## Test Plan

Windows-only integration tests should cover:

- server provider creates a pipe and accepts one client;
- client provider succeeds when the server is already available;
- roundtrip read/write between `NamedPipeListen` and `NamedPipeConnect`;
- `NamedPipeConnect` retries `NotFound` until the server appears;
- `NamedPipeConnect` classifies raw `ERROR_PIPE_BUSY` as retryable during provider init;
- runtime `NamedPipeConnect::connect()` call sites retry raw `ERROR_PIPE_BUSY`
  where short listener-replacement windows are expected;
- `PermissionDenied` is fatal where practical to trigger deterministically;
- generated non-Windows guard emits a clear compile error at the provider
  declaration site.

Linux/macOS CI can still run macro parser trybuild coverage for accepted syntax
and non-Windows compile errors. Runtime named pipe tests must run on Windows
MSVC first; Windows GNU support should remain a separate linkme/toolchain
decision.

[Back to README](../../README.md)
