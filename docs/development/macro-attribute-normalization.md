# Macro Attribute Normalization

This note records the maintainer contract for cleaning up
`service-daemon-macro` attribute parsers. It is about parser implementation
boundaries, not a new public macro syntax.

The public macro forms remain the source of truth:

```rust
#[service(priority = 80, scheduling = HighPriority, tags = ["infra"])]
#[trigger(TT::Queue(JobQueue), priority = 60)]
#[provider(8080, env = "PORT")]
#[provider(default = 8080, env = "PORT")]
#[provider(Queue(String), capacity = 128)]
#[provider(template = Queue(String), capacity = 128)]
#[provider(Listen("127.0.0.1:8080"), eager = true)]
```

## Parser Pipeline

Parser cleanup should follow this pipeline:

```text
custom macro head parser -> shared named-tail parser -> canonical internal model -> semantic validation -> codegen
```

The pipeline exists so the user-facing syntax can stay ergonomic while the macro
crate has one place to reason about common named attributes and ambiguity rules.

## Macro Shapes

`#[service(...)]` has no custom head. It is a named-tail-only macro using common
entry attributes:

- `priority = ...`
- `scheduling = ...`
- `tags = [...]`

Auto-start is not a service attribute. It is inferred from the service
signature: no `#[input]` parameter means the selected entry is auto-started,
while one `#[input] value: &T` parameter means the selected entry is a template
service. `ServiceHandle::create(input)` and `ServiceHandle::start(input)` accept
owned `T` and the generated wrapper passes `&T` to each generation.

`#[trigger(Host(Target), ...)]` has a custom head and a common named tail. The
host is parsed as an open `syn::Path` and emitted into the `TriggerHost<Target>`
contract. Do not add an internal trigger-host name registry. `TT::*` is the
built-in alias namespace, not the parser's authority.

`#[provider(...)]` has an optional provider head and a provider-specific named
tail. The shorthand head slot intentionally has two meanings:

- default expression, such as `8080`, `"localhost"`, or `make_config()`;
- built-in provider template shorthand, such as `Notify`, `Queue(String)`, or
  `Listen("127.0.0.1:8080")`.

The same heads may also be written explicitly as named heads:

- `default = 8080`
- `template = Queue(String)`

The shorthand and explicit forms are public equivalents. Both normalize to the
same internal `ProviderHead` model before semantic validation and codegen.

Provider templates are currently macro-crate built-ins. The central provider
parser may classify a head as built-in template sugar using `TEMPLATE_NAMES`,
but arbitrary `Path(...)` is not an open provider-template signal.

## `syn::meta::ParseNestedMeta`

Use `syn::meta::ParseNestedMeta` only where its model matches the syntax: named
tail arguments that start with a path and usually continue with `= value`.

Good fits:

- `priority = 80`
- `scheduling = HighPriority`
- `tags = ["infra"]`
- `env = "PORT"`
- `capacity = 128`
- `eager = true`

Do not make `ParseNestedMeta` the whole parser for provider heads. A bare
default expression like `8080` or `"localhost"` is not a meta item beginning
with a path, and provider template shorthand needs provider-specific ambiguity
rules.

## Internal Models

Use small canonical models before codegen consumes parsed attributes:

- `CommonEntryAttrs` for service and trigger named tails.
- `ProviderHead` for provider heads:
  - `Empty`
  - `DefaultExpr`
  - `BuiltinTemplate`
- `ProviderNamedAttrs` for provider named tails:
  - `env`
  - `capacity`
  - `eager`

These models are internal. The public API supports both the ergonomic shorthand
heads and the explicit head keys, but users should not rely on internal model
names beyond the documented `default = ...` and `template = ...` spellings.

## Migration Order

1. Move service named attributes onto the shared named-tail parser.
2. Move trigger named tails onto the same helper while keeping the custom
   `Host(Target)` head parser.
3. Normalize provider parsing into `ProviderHead` and `ProviderNamedAttrs`
   while preserving shorthand heads and accepting explicit `default = ...` /
   `template = ...` head keys as equivalent forms.
4. Add focused compile-fail fixtures before moving each built-in provider
   template's argument parsing behind the template implementation.
5. Stop when the shared model removes real drift and diagnostics are stable.

Do not continue abstraction work only because a generic parser can express a
shape. Continue only when it removes duplication, clarifies a specific
ambiguity rule, or makes diagnostics easier to maintain.

## Compatibility Rules

Parser normalization must not change generated registry entries, generated
helper signatures, linkme registry emission, provider initialization semantics,
or runtime contracts.

Treat trybuild diagnostics as the compatibility gate. Preserve current error
text and span ownership for duplicate attributes, unknown attributes, invalid
provider heads, invalid template arguments, unsafe provider functions, trigger
head shape errors, and service named-tail errors unless a diagnostic change is
explicitly reviewed.

[Back to README](../../README.md)
