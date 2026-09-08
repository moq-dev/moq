The clusterable, media-agnostic relay. It routes by broadcast path and never inspects payloads; anything media-aware belongs in a gateway or `moq-mux`.

# Config

Config structs are clap `Parser`s that also load from TOML, with the `moq-native` configs flattened in.

- Every `#[arg]` field on a TOML-loadable config must be `Option<T>`, never a bare `bool`/`String`/number. The TOML -> CLI merge re-applies clap defaults, so a bare field silently clobbers the TOML value. Add a `cli_does_not_clobber_toml_*` test in `config.rs` for every new flag.
- Relay behavior and config changes update `doc/bin/relay/`. Stats track names and frame shapes are documented in `doc/bin/relay/config.md`.

# Testing

`tests/drills.rs` disrupts a live session through a real relay over real QUIC: a cancelled reader with a backlog, the relay dying mid-group, and a publisher republishing a name it lost. Each drill records that its fault activated and requires a terminal result rather than a clean finish or a hang. `test/drill/README.md` covers the recipe and the loom and fuzz cases underneath.

# Semver

Relay patch bumps only cover breaking config changes; release-plz owns every version field.
