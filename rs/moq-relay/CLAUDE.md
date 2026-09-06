The clusterable, media-agnostic relay. It routes by broadcast path and never inspects payloads; anything media-aware belongs in a gateway or `moq-mux`.

# Config

Config structs are clap `Parser`s that also load from TOML, with the `moq-native` configs flattened in.

- Every `#[arg]` field on a TOML-loadable config must be `Option<T>`, never a bare `bool`/`String`/number. The TOML -> CLI merge re-applies clap defaults, so a bare field silently clobbers the TOML value. Add a `cli_does_not_clobber_toml_*` test in `config.rs` for every new flag.
- Relay behavior and config changes update `doc/bin/relay/`. Stats track names and frame shapes are documented in `doc/bin/relay/config.md`.

# Semver

Relay patch bumps only cover breaking config changes; release-plz owns every version field.
