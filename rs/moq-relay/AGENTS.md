The clusterable, media-agnostic relay: it routes by broadcast path and never inspects payloads. Media-aware logic belongs in a gateway or `moq-mux`.

- A relay never blocks an answer on another node finishing something (replay, catch-up, a full announce set); it reports what it knows now.
- Config merges CLI > env > file > defaults via `moq_tokio::cli::Merge`. Presence comes from the source, never from whether a value looks empty. A new merged flag needs `setting = "dotted.key"` and a matching `usage_rs::Config` field in `settings.rs`.
- `tests/drills.rs` disrupts live sessions over real QUIC; each drill proves its fault fired and requires a terminal result, not a clean finish or a hang. Add a mutation in `test/drill/README.md` for any recovery behavior a new drill grades.
