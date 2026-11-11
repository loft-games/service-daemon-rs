# Windows Named Pipe IPC Provider Contract

This note records why the Windows named pipe provider contract keeps explicit
provider templates for native named-pipe control, and how the cross-platform
`LocalIpc` facade layers logical names over that contract.

## Provider Contract

The public templates are intentionally explicit:

```rust
#[provider(NamedPipeListen(r"\\.\pipe\myapp"))]
pub struct ControlPipe;

#[provider(NamedPipeConnect(r"\\.\pipe\myapp"), env = "CONTROL_PIPE", eager = true)]
pub struct ControlClient;
```

The pipe argument and shared `env` attribute accept either a string literal or a
path to a `const`/`static &'static str`. Dynamic string expressions are rejected
by the macro.

`NamedPipeListen` owns the server side. It stores the local pipe name, the first pending `NamedPipeServer`, and a lazily started listener manager. `try_new()` creates the first instance with `reject_remote_clients(true)` and `first_pipe_instance(true)` so startup fails if another server already owns the pipe name. `accept().await` receives an already connected server end from that manager. After each connection, the manager creates the next instance without `first_pipe_instance(true)`; if replacement creation fails, it retries internally with short backoff instead of failing the business `accept()` call.

`NamedPipeConnect` owns only the local pipe name. `try_new().await` validates the
pipe name and stores it without dialing the peer. `connect().await` opens a fresh
independent `IpcStream` and retries short `ERROR_PIPE_BUSY` windows internally.
If a retry loop has already observed `ERROR_PIPE_BUSY`, transient `NotFound`
results are treated as the same listener-replacement gap and retried within the
same bounded retry window.

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

## LocalIpc Facade

`LocalIpcListen(Name)` and `LocalIpcConnect(Name)` provide the cross-platform
facade for local byte-stream IPC. They accept a logical name, not a platform
endpoint. On Windows that logical name maps to
`\\.\pipe\service-daemon-rs-<name>` and reuses the named-pipe local-only,
first-instance ownership and listener manager semantics described above.

This does not replace `NamedPipeListen` or `NamedPipeConnect`. Use the explicit
Windows templates when code or deployment needs a specific pipe path. Use
`LocalIpc*` when the service only needs a local stream shape shared with Unix,
typically written against `AsyncRead + AsyncWrite + Unpin`.
