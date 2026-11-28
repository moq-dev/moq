# [M] Unify moq_mux::catalog consumers on Timed and hide internal hang::Container audio variants

## Goal

Every `moq_mux::catalog` consumer (`hang::Consumer`, `msf::Consumer`, `catalog::Consumer`, `catalog::Stream`, and `Select`) yields `moq_json::Timed<Catalog<E>>`, `Guard::stamp`/`retime` validates buffered edits adopting `sync.clock` against the rebased floor rather than across two different clocks, and `moq_mux::catalog::hang::Container` hides its internal codec-duration state instead of exposing an exhaustive enum with `*Audio(legacy::Audio)` variants.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit):
- ✅ Unify all `moq_mux::catalog` consumers (`msf::Consumer`, `catalog::Consumer`, `Stream`, `Select`) to yield `Timed<Catalog<E>>` (rejected: reverting `hang::Consumer` to bare `Catalog<E>` and having `moq-transcode` read `hang::catalog::Consumer`).
- ✅ Clean up `moq_mux::catalog::hang::Container` before `0.22.0` so adding audio duration support to another container kind never breaks exhaustive matches (rejected: leaving `*Audio(legacy::Audio)` public and exhaustive).

Caused by [moq-dev/moq#9714](https://github.com/moq-dev/moq/pull/9714) and [moq-dev/moq#9553](https://github.com/moq-dev/moq/pull/9553).

1. **Unified `Timed<Catalog<E>>` across `moq_mux::catalog` (`#9714`):**
   - Update `rs/moq-mux/src/catalog/msf/consumer.rs:41,55` (already has `frame.timestamp`), `rs/moq-mux/src/catalog/consumer.rs:118-121`, `rs/moq-mux/src/catalog/stream.rs:30`, and `rs/moq-mux/src/catalog/select.rs:33` to return `moq_json::Timed<Catalog<E>>`, matching `hang::catalog::Consumer`, `moq_mux::catalog::hang::Consumer`, and `MoqCatalog { timestamp }`.
   - Update in-tree callers and record the breaking migration note in `## Impact`.
2. **Fix `Guard::stamp` / `retime` cross-clock comparison (`#9714`):**
   - In `rs/moq-mux/src/catalog/producer.rs:684-688,1266-1338`, `Producer::modify` currently calls `guard.stamp(timestamp)` before the caller writes `sync.clock`, comparing a new-clock timestamp against an old-clock `Gate::Buffering { at: Some(floor), .. }`, and `Guard::publish` then rebases `staged` via `retime()` and overwrites `*staged = at.or(*staged)` without verifying `at >= rebased_staged`.
   - Defer validation of `at` until `Guard::publish` (after `retime()` has placed `staged` on the adopted clock), so valid clock-adopting edits never fail with spurious `TimestampRewind` and out-of-order stamps never move `staged` backwards.
3. **Hide internal `*Audio` variants in `moq_mux::catalog::hang::Container` (`#9553`):**
   - In `rs/moq-mux/src/catalog/hang/container.rs:6-28` (`rs/moq-mux/src/catalog/hang/mod.rs:14`), collapse `LegacyAudio`/`RawAudio`/`CtsAudio` into an opaque struct wrapping its private enum (mirroring JS `Format` with private `#duration`), or keep `Container` as a `#[non_exhaustive]` enum without leaking `legacy::Audio` variants.

Tests: unit tests covering `msf::Consumer` and `catalog::Consumer`/`Stream`/`Select` returning timestamps, a `Producer::modify` test adopting a new `sync.clock` while `Gate::Buffering` holds an earlier stamp on the old clock (both valid adoption and rewind rejection), and container round-trip tests.

Public API: breaking change in `moq-mux` (unified `Timed<Catalog<E>>` return types and `hang::Container` shape; PR title uses `!` and `## Impact` carries the migration note). Wire: none.
