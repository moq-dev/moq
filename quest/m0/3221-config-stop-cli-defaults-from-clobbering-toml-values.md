# [M] config: stop CLI defaults from clobbering TOML values

## Goal

Implement and verify the behavior tracked in [#3221](https://github.com/moq-dev/moq/issues/3221)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

Target branch: `dev`.

`rs/moq-relay/src/config.rs::Config::parse_and_merge` parses the CLI, loads the selected TOML file, then calls `config.update_from(&args)` so explicit CLI and environment values win. The final clap parse also reapplies `default_value` and `default_value_t` entries when the user did not supply those arguments, so defaults silently overwrite file values.

Known affected fields include:

- `moq_native::ClientConfig::bind`
- `moq_native::Backoff::{initial, multiplier, max, timeout}`
- `moq_native::websocket::Client::{enabled, delay}`
- `moq_native::Log::level`
- `moq_relay::WebConfig::ws`

The existing `Option<T>` convention only protects fields whose clap declaration has no default. For example, `websocket.delay` is optional but still has `default_value = "200ms"`, so a TOML value is replaced during `update_from`.

#### Failure example

Given a TOML file with values such as:

```toml
[web]
ws = false

[client]
bind = "127.0.0.1:12345"

[client.websocket]
enabled = false
delay = "3s"

[client.backoff]
initial = "7s"
multiplier = 3
max = "30s"
timeout = "2m"

[log]
level = "debug"
```

running `moq-relay <file>` without matching CLI overrides resets those fields to clap defaults. The most serious example is `web.ws = false` becoming `true`, which can expose a listener mode the operator explicitly disabled.

#### Expected

Only values explicitly supplied through CLI arguments or environment variables should override TOML. Parser defaults should fill fields only when no source provided a value.

#### Suggested direction

Fix the merge centrally instead of relying on every flattened config field to use a particular Rust type. Possible shapes are an all-optional CLI overlay, or using clap value-source metadata so only command-line and environment values are applied after TOML deserialization.

Add table-driven regression coverage for nested flattened structs, bare scalar defaults, optional fields with clap defaults, environment overrides, and explicit CLI overrides.

## Closes

- [#3221](https://github.com/moq-dev/moq/issues/3221) - close this issue when the quest finishes
