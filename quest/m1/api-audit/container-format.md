# [M] Container formats name inputs

## Goal

`moq_mux::import::ContainerFormat` names only what is parsed. How the import
is packaged (per-sample frames or preserved fragments) is never chosen by an
input-sounding name, and the format strings mean one thing each.
`ContainerStream` and the TS passthrough internals are not public.

## Plan

Decided 2026-10-22 (API audit). #6250 made `Fmp4` unwrap to per-sample frames
and added `Cmaf` for fragment passthrough over the same input
(`rs/moq-mux/src/import/format.rs`, `container.rs`). That flip was
deliberate; the defect is the naming axis. `"cmaf"` means passthrough while
`"m2ts"` means demux to TS, though the catalog's `m2ts` section means TS
passthrough, and `ts::Passthrough` is unreachable from `ContainerFormat`.

- Delete `ContainerFormat::Cmaf` from Rust, moq-ffi, moq-c
  (`MOQ_CONTAINER_FORMAT_CMAF`), and every wrapper enum; none has a caller.
  Parse `"cmaf"` as `Fmp4`. Passthrough stays Rust-only through
  `fmp4::Import::with_passthrough` and `moq_hls::import::Config::passthrough`,
  which the CLI already uses. The Impact bullet supersedes the 8 binding
  MIGRATE entries #6250 wrote, for the cut to collapse. Chosen over a `ContainerInit::passthrough` flag because nothing
  outside Rust asks for passthrough yet.
- Make the `"m2ts"` string agree with the catalog meaning, or rename it so it
  does not collide; verify against callers first.
- Delete `ContainerStream` (`import/container.rs`, inherited, a line-for-line
  copy of `Container`); `Container::new` with empty `ContainerInit::data` is
  equivalent. Keep `Track` and `TrackStream`. Fix `doc/lib/layers.md`.
- TS passthrough (`container/ts/mod.rs`, `passthrough.rs`): make the module
  private and export only `Passthrough`; fold its four error structs into
  `moq_mux::Error` instead of `anyhow`; consider
  `ts::passthrough::{Import, Export}` for the `Passthrough`/`Release` pair.
- Make `Container::cut()` and `fmp4::Import::discontinuity()` return
  `Result` like `Track`'s.

Public API: breaking in moq-mux, moq-ffi, moq-c, and wrappers. Wire: none.
