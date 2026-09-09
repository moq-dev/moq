# [L] Config: track which source set each value

## Goal

`moq-relay` and `moq-bench` resolve configuration with recorded provenance.
Only a value the command line or the environment actually supplied overrides
the TOML file: a list the file sets to empty survives the merge, and a secret
injected by an orchestrator beats a placeholder in a checked-in file.
Precedence is `CLI > env > file > defaults`, declared in one place, and the
relay can say which source set a setting.

## Plan

`Config::parse_and_merge` (rs/moq-relay/src/config.rs:197-233) parses CLI plus
environment plus defaults, overlays the TOML file, then re-parses argv with
`update_from` so explicit flags win. Presence is inferred from the standing
value, so the re-parse refills whatever reads as empty:

- A `Vec<T>` reads empty when it has no items (usage-derive 6.3.0
  `standing_presence`, codegen.rs:3591), so a file that deliberately sets
  `version = []` is refilled from the environment. About fifteen env-bound
  list fields are affected: `moq-tokio`'s TLS roots, fingerprints, certs,
  keys, and generated names (rs/moq-tokio/src/tls.rs:435,471,1211,1218,1230,
  1277), the accepted versions on both sides (listen.rs:73, connect.rs:495),
  the Unix peer allowlists (unix.rs:150,160,171), and the relay's cluster
  peers, auth roots and domains, and web HTTPS material
  (rs/moq-relay/src/cluster.rs:419, auth.rs:323,472, web.rs:110,124,143).
- A plain scalar always reads as present (codegen.rs:3589), so a flag with a
  declared default is safe. A bare `bool` is not, which is why every
  TOML-overridable boolean is typed `Option<bool>` and resolved in code
  (`web.ws`, rs/moq-relay/src/web.rs:62; the regression at config.rs:487-491).
  That convention is a per-field workaround; `Option<Vec<T>>` would multiply
  it.
- The file outranks the environment, which no comparable tool does.

Fix the merge instead of the types:

- Enable the `config` feature of the workspace `usage` dependency
  (Cargo.toml:201; usage-rs 6.3.0 already ships `usage::Config` and
  `usage::config`). Build the layers from what the parser saw rather than
  from the parsed struct: `usage::config`'s `CliLayer`, `EnvLayer` over a
  `Registry` that declares which key each variable backs, and `Layers` in
  caller-chosen order give exactly that, plus provenance. The cost is real:
  `usage::Cli` and `usage::Config` reject each other's attributes, so every
  merged setting is declared a second time: 44 relay flags plus the 100
  flattened in from `moq-tokio` (`long =` across rs/moq-relay/src and
  rs/moq-tokio/src), and moq-bench's 13 own settings plus its three flattened
  groups (rs/moq-bench/src/config.rs:81-93). `Registry::drift` is the test
  that the two declarations stay in step. An all-optional overlay struct is
  the fallback if the registry proves too heavy; either way, presence comes
  from the source, never the value.
- Flip precedence to `CLI > env > file > defaults` and state it in
  `doc/bin/relay/config.md`, which today only says every key is also a flag
  and an environment variable (:8).
- Delete the `Option<bool>` resolve-in-code convention once the merge no
  longer needs it, so a plain field is safe again.
- Table-driven regression coverage: empty lists, false booleans, optional
  fields with declared defaults, nested flattened structs, environment
  overrides, and explicit CLI overrides, each asserting both the value and
  its source.

Branch from `dev`: the relay config lives on `usage` there.

## Closes

- [#3051](https://github.com/moq-dev/moq/issues/3051) - close this issue when the quest finishes
- [#3221](https://github.com/moq-dev/moq/issues/3221) - the list half is what remains: a file's empty list is still refilled from the environment
