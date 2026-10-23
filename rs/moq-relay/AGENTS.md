The clusterable, media-agnostic relay. It routes by broadcast path and never inspects payloads; anything media-aware belongs in a gateway or `moq-mux`.

A relay never blocks an answer on another node finishing something (a replay, a catch-up, a complete announce set); it reports what it knows now and accepts the rare race.

# Config

Config structs are Usage `Args` that also load from TOML, with the `moq-tokio` configs flattened in.

- Merge is CLI > env > file > defaults, declared in `moq_tokio::cli::Merge`. Presence comes from the source (`CliLayer` / `EnvLayer` / the TOML document), never from whether a standing value looks empty. A new merged flag needs `setting = "dotted.key"` on the `Args` field and a matching `usage_rs::Config` field in `settings.rs`; `Registry::drift` is the test they stay in step.

# Testing

`tests/drills.rs` disrupts a live session through a real relay over real QUIC. Each drill records that its fault activated and requires a terminal result rather than a clean finish or a hang. `test/drill/README.md` covers the recipe and the sensitivity proof; add a mutation there for any recovery behavior a new drill grades.
