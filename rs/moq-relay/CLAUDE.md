The clusterable, media-agnostic relay. It routes by broadcast path and never inspects payloads; anything media-aware belongs in a gateway or `moq-mux`.

# Config

Config structs derive `Parser`, `Serialize`, `Deserialize` with `#[serde(deny_unknown_fields, default)]`, clap `#[arg(long, env = "MOQ_...")]`, nested sections via `#[command(flatten)]`, and an `init()`/`load()` that produces the live object. The same shape is used by the `moq-native` configs flattened into it.

- Every `#[arg]` field on a TOML-loadable config must be `Option<T>`, never a bare `bool`/`String`/number. The TOML -> CLI merge re-applies clap defaults, so a bare field silently clobbers the TOML value. Add a `cli_does_not_clobber_toml_*` test in `config.rs` for every new flag; those tests serialize env mutation with a lock because clap reads env.
- A deprecated flag stays a hidden clap alias, never a `--help` entry. Warn-then-ignore is banned: a flag is supported or refused.
- Relay behavior and config changes update `doc/bin/relay/`. Stats track names and frame shapes are documented in `doc/bin/relay/config.md`.

# Semver

Relay patch bumps only cover breaking config changes; release-plz owns every version field.
