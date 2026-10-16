# [M] moq-json groups roll on demand

## Goal

moq-json and `@moq/json` snapshot and window producers roll a new group only
when a joiner wants one, or at the existing hard caps, instead of whenever
deltas reach `delta_ratio` times the snapshot (or ops reach `op_ratio`
times the window). Both are renamed `refresh` in Rust, JS, moq-ffi, and
every binding. It defaults to 8, and the
producer advertises it as `track::Info::refresh`.

## Plan

Decided 2026-10-16 (see the [epic](/quest/m1/refresh/README.md)).

- `snapshot::Config::refresh` replaces `delta_ratio` (Rust
  `with_delta_ratio`, JS `deltaRatio`), and `window::ProducerConfig::refresh`
  replaces `op_ratio` (Rust `with_op_ratio`, JS `opRatio`). Both keep their
  unit and default (8). The producer sets
  `track::Info::refresh` from it, so the model and relays apply the same
  ratio.
- The producer awaits `Demand::wanted`. When it passes the largest group,
  the producer re-emits the current value as a snapshot in a new group
  immediately, rather than waiting for the next update.
- The unconditional delta-budget roll goes away. `MAX_DELTA_FRAMES` and the
  group byte cap still force a snapshot. They are the bound behind a hop
  that cannot forward requests, so a joiner there downloads at most up to
  the caps.
- `refresh: None` disables refresh. Groups then roll only at the caps,
  unless `cut()` is called.
- This works locally without wire support: a local subscriber's join
  drives `wanted` through [core](/quest/m1/refresh/core.md) alone.
- In-repo users follow: the hang and moq-mux catalog producers,
  `js/publish` catalog, the moq-room and `js/room` chat windows, the
  moq-hls and moq-archive tests, benches, and examples.
- Rename in moq-ffi (`rs/moq-ffi/src/json.rs`), moq-c, the py, go, kt, and
  dart wrappers, `doc/lib/*`, the moq-json and `@moq/json` READMEs, and the
  Unreleased section of `doc/setup/upgrade.md`.
- moq-flate is unchanged. Its snapshot mode already opens a group per
  update, and its stream mode keeps one group by design.

Tests: under simulated time, a joiner below the ratio gets the current
group, a joiner above it gets a fresh snapshot group, with no joiner the
group grows to the caps, and `refresh: None` never rolls early. Run in Rust
and JS.

Public API: breaking (`delta_ratio` and `op_ratio` renamed to `refresh` in
Rust, JS, and every binding that exposes them). Wire: none.

## Required

- [Core](/quest/m1/refresh/core.md) - `Demand::wanted` and `track::Info::refresh`

## Related

- [JSON window timestamps](/quest/m1/json-window-timestamps.md) - touches the same window code
- [Data consumer timestamps](/quest/m1/data-consumer-timestamps.md) - touches the same snapshot consumers and bindings
