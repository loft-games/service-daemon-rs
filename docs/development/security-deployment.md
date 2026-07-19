# Security and Deployment Contract

This maintainer note records deployment boundaries that the framework does not
enforce automatically. It is documentation-first: changing these rules should be
handled separately from runtime behavior changes.

## Scope

The framework provides service orchestration, dependency providers, triggers,
status reporting, and optional file logging. It does not provide application
authentication, TLS termination, request authorization, secret redaction,
network perimeter controls, host hardening, or operating-system service
installation.

Treat examples as framework-topology references. They show how to wire services,
providers, triggers, and shutdown behavior; they are not production deployment
templates.

## Logging Safety

Framework logging does not redact sensitive data automatically. Structured
fields, messages, targets, source locations, and captured error chains are
rendered as supplied by the application and dependencies. Application code is
responsible for keeping passwords, tokens, API keys, authorization headers,
connection strings, session identifiers, and personal data out of log events.

Prefer logging stable non-secret identifiers, coarse status fields, counts, and
error categories. If an external error can contain a secret-bearing URL, header,
or request body, sanitize it before attaching it to `tracing` fields or returning
it through an error path that may be recorded.

### File Logging

The `file-logging` feature is best-effort persistence for operational logs, not
a strong audit trail:

- if the rolling file appender cannot initialize, the framework logs a warning
  and continues with console logging only;
- if a file-log consumer lags behind the broadcast queue, some events may not be
  persisted to file;
- file ownership, directory permissions, disk retention, backup, shipping, and
  tamper resistance are deployment responsibilities.

For audit-grade logging, deploy an external log pipeline with explicit
durability, access control, retention, and monitoring guarantees.

## Unix Socket Deployment

`UnixListen` and `UnixConnect` are Unix-only local IPC templates. A Unix socket
path is part of the deployment security boundary because the parent directory
controls who can create, replace, or connect to the socket.

For production deployments:

- place sockets under a private runtime directory such as `/run/<app>/`;
- make the directory owned by the service user or service group;
- use restrictive directory permissions, typically `0700` for one service user
  or `0750` when a service group needs access;
- avoid shared world-writable directories such as `/tmp` for production socket
  paths;
- prefer an environment override for deployment-specific paths instead of
  baking host paths into source code.

The framework's stale-socket cleanup is intentionally conservative: it should
not turn a shared directory into a safe production boundary. In a shared
directory, another local user can create race conditions, confusing leftover
paths, or denial-of-service conditions around the socket name.

## TCP Listener Exposure

`Listen` binds the configured address early during provider initialization. The
bind address is a deployment decision:

- use loopback addresses such as `127.0.0.1:<port>` for local-only services;
- bind externally only when firewall policy, authentication, authorization,
  rate limiting, TLS or reverse-proxy controls, request-size limits, and
  observability are already designed;
- review every environment variable that can override a bind address before
  exposing it in production.

Examples may use `Listen` to demonstrate framework wiring. They do not imply
that the framework supplies HTTP security controls for production APIs.

## Example Boundaries

`examples/web-api` is an adoption reference for Axum integration, request
envelopes, CORS wiring, OpenAPI documentation, maintenance triggers, and graceful
shutdown. It is not a production-ready API server template. In particular,
production users must provide their own authentication, authorization, input
policy, CORS policy, TLS, request limits, abuse protection, and deployment
hardening.

`examples/unix-domain-socket` uses a `/tmp` socket path so it is easy to run
locally. That path is not production-safe; production deployments should use a
private runtime directory as described above.

Other examples should be read according to the example-layer table in
`docs/development/release-validation.md`.
