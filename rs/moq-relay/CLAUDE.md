The clusterable, media-agnostic relay. It routes by broadcast path and never inspects payloads; anything media-aware belongs in a gateway or `moq-mux`.

# Config

Config structs are Usage `Args` that also load from TOML, with the `moq-tokio` configs flattened in.

- Merge is CLI > env > file > defaults, declared in `moq_tokio::cli::merge`. Presence comes from the source (`CliLayer` / `EnvLayer` / the TOML document), never from whether a standing value looks empty. A new merged flag needs `setting = "dotted.key"` on the `Args` field and a matching `usage::Config` field in `settings.rs`; `Registry::drift` is the test they stay in step.
- Relay behavior and config changes update `doc/bin/relay/`. Stats track names and frame shapes are documented in `doc/bin/relay/config.md`.

# Testing

`tests/drills.rs` disrupts a live session through a real relay over real QUIC: a cancelled reader with a backlog, the relay dying mid-group, and a publisher republishing a name it lost. Each drill records that its fault activated and requires a terminal result rather than a clean finish or a hang. `test/drill/README.md` covers the recipe and the sensitivity proof; add a mutation there for any recovery behavior a new drill grades.

# Semver

Relay patch bumps only cover breaking config changes; release-plz owns every version field.
