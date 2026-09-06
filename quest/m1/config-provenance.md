# [L] Config: track which source set each value

## Goal

`moq-relay` and `moq-bench` resolve configuration with recorded provenance.
Only a value the command line or the environment actually supplied overrides
the TOML file; a list the file sets to empty, a flag it sets to false, and a
field the parser merely defaults all survive the merge. Precedence is
`CLI > env > file > defaults`, declared in one place, and the relay can say
which source set a setting.

## Plan

Two shipped defects share one cause. The merge parses CLI plus environment
plus defaults, overlays the file, then re-parses argv so explicit flags win.
Presence is inferred from the value, so "set to the empty list", "set to
false", "set to the declared default", and "never set" are indistinguishable:

- A `Vec<T>` reads empty when it has no items, so a file that deliberately
  sets `version = []` is refilled from the environment. Roughly fifteen
  env-bound list fields are affected: TLS certificate, key, and root lists,
  accepted versions on both sides, the Unix peer allowlists, cluster peers,
  auth domains, and the web HTTPS material.
- The final re-parse reapplies every declared default, so a flag with a
  default (`websocket.delay = "200ms"`, `web.ws`, the backoff fields, the log
  level, the client bind) overwrites what the file said. `web.ws = false`
  becoming `true` re-exposes a listener the operator disabled.
- The file outranks the environment, which no comparable tool does and which
  means a secret injected by an orchestrator cannot override a placeholder in
  a checked-in file.

The `Option<bool>` convention that patched the boolean half is a per-field
workaround; `Option<Vec<T>>` would multiply it. Fix the merge instead of the
types:

- Build the layers from what the parser saw rather than from the parsed
  struct. `usage-config`'s `CliLayer`, `EnvLayer` over a registry that
  declares which key each variable backs, and `Layers` in caller-chosen order
  give exactly that, plus provenance. The cost is real: `usage::Cli` and
  `usage::Config` reject each other's attributes, so every merged setting is
  declared a second time, about a hundred for the relay and twenty for the
  bench, with `Registry::drift` as the test that the two declarations stay in
  step. An all-optional overlay struct is the fallback if the registry proves
  too heavy; either way, presence comes from the source, never the value.
- Flip precedence to `CLI > env > file > defaults` and state it in
  `doc/bin/relay/config.md`, replacing the sentence that describes today's
  emergent order.
- Delete the `Option<bool>` resolve-in-code convention once the merge no
  longer needs it, so a plain field is safe again.
- Table-driven regression coverage: empty lists, false booleans, optional
  fields with declared defaults, nested flattened structs, environment
  overrides, and explicit CLI overrides, each asserting both the value and
  its source.

Branch from `dev`: the relay config lives on `usage` there.

## Closes

- [#3051](https://github.com/moq-dev/moq/issues/3051) - close this issue when the quest finishes
- [#3221](https://github.com/moq-dev/moq/issues/3221) - close this issue when the quest finishes
