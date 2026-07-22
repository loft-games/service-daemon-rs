# Windows Named Pipe IPC Provider Contract

This note records why the Windows named pipe provider contract adds explicit provider templates instead of changing the existing Unix socket templates or introducing a cross-platform facade.

## Provider Contract

The public templates are intentionally explicit:

```rust
#[provider(NamedPipeListen(r"\\.\pipe\myapp"))]
pub struct ControlPipe;

#[provider(NamedPipeConnect(r"\\.\pipe\myapp"), env = "CONTROL_PIPE", eager = true)]
pub struct ControlClient;
```

`NamedPipeListen` owns the server side. It stores the local pipe name, the first pending `NamedPipeServer`, and a lazily started listener manager. `try_new()` creates the first instance with `reject_remote_clients(true)` and `first_pipe_instance(true)` so startup fails if another server already owns the pipe name. `accept().await` receives an already connected server end from that manager. After each connection, the manager creates the next instance without `first_pipe_instance(true)`; if replacement creation fails, it retries internally with short backoff instead of failing the business `accept()` call.

`NamedPipeConnect` owns only the local pipe name. `try_new().await` performs one reachability probe with `ClientOptions::new().open(...)` and drops the probe. `try_connect().await` and `connect().await` open fresh independent `NamedPipeClient`s. A business `connect()` can briefly observe raw `ERROR_PIPE_BUSY` while the peer listener replenishes the next server instance after the init probe; callers that expect that handoff should retry the busy result with a short bounded wait.

Both templates accept the same top-level shared provider attributes as other address templates: `env` and `eager`. Tuning attributes such as pipe mode, buffer sizes, ACL/security descriptors, maximum instances, QoS flags, and raw security attributes are deliberately outside the first contract.

## Unix Sockets Stay Unix-only

`UnixListen` and `UnixConnect` continue to mean Unix domain sockets through `std::os::unix::net` and Tokio Unix APIs. Their behavior depends on filesystem socket paths: stale socket-file detection, safe unlinking, and Unix-specific path errors. Mapping those names to Windows named pipes would make the same macro syntax mean different kernel objects and different failure semantics on different targets.

For that reason, Unix socket declarations still require `#[cfg(unix)]` in cross-platform crates. On non-Unix targets the macro emits a targeted compile error.

## Why Not Windows AF_UNIX In This Phase

Windows has AF_UNIX support, but it is not the right baseline for this provider model today:

- The existing Unix templates are built around `std::os::unix` and Unix socket-file lifecycle rules, which are not portable Rust APIs on Windows.
- Windows AF_UNIX deployment support varies by OS version and runtime environment, while named pipes are the native Windows local IPC primitive.
- The framework needs ownership detection and local-only IPC semantics. Named pipes expose an explicit first-instance ownership flag and remote-client rejection that match that contract directly.

Windows AF_UNIX may be useful for application-specific code, but this framework contract uses named pipes for the Windows side.

## Local-only Named Pipes

The Windows named pipe provider contract accepts only local pipe paths beginning with `\\.\pipe\` and rejects empty suffixes. Remote forms such as `\\server\pipe\name` are fatal configuration errors. The server also requests `reject_remote_clients(true)`.

This keeps the first version focused on local daemon/sidecar IPC. Remote named pipes, custom ACLs, explicit security descriptors, and QoS tuning need a separate API review because they change deployment and security expectations.

## No LocalIpc Facade Yet

The phase does not add `LocalIpcListen` or `LocalIpcConnect`. A later facade can be considered only after Unix and Windows contracts have enough real usage to compare method shape, error taxonomy, ownership, and security needs without hiding important platform differences.
