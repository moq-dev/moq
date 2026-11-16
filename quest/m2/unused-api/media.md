# [M] The media and gateway packages publish no API without a caller

## Goal

The media crates and browser packages the cut publishes export nothing that
only their own tests call. Each item below is deleted, made crate-private,
or marked `@internal`, in Rust and JS together where both have it, unless
the PR finds a downstream caller, in which case it stays and the PR says
who calls it.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Remove or hide every unused public item, one sweep per area, each
  re-checked for a downstream caller first (rejected: ship them; decide
  item by item in the [API review](/quest/m3/release/api-review.md) after
  the cut, when each removal is a break). Publishing later is additive;
  removing later is not.

Before touching an item, search Kixel and moq.pro for a caller (the
maintainer's condition) and keep what one uses. The greps below cover this
repository only, at `80ca9787e3`.

Rust:

- `moq_mux::container::fmp4::Fragmenter`, `fragment::Config`, and
  `Muxer::fragmenter` (`rs/moq-mux/src/container/fmp4/fragmenter.rs`, 644
  lines; `muxer.rs:330`): delete. `grep -rln 'Fragmenter\|fragmenter()' rs`
  finds only four files in `container/fmp4/`, and the callers are the
  type's own tests. Drop its bullet from
  [the fMP4 group start](/quest/m3/fmp4-group-start-tfdt.md), which lists
  `Fragmenter::emit` as a site to fix.
- `moq_video::encode::Applied`, `Encoder::applied`
  (`rs/moq-video/src/encode/encoder.rs:83`, `:437`), `Sink::applied`
  (`rs/moq-video/src/encode/sink.rs:82`), and
  `moq_transcode::active::Rendition::applied`
  (`rs/moq-transcode/src/active.rs:124`): crate-private.
  `grep -rn '\.applied()' rs` finds callers only inside moq-video, one
  setter in `rs/moq-transcode/src/rung.rs:1148`, and a test (`:1630`,
  `:1635`). `Applied` also does three jobs in one struct: a confirmed
  preset that sets the catalog's `reorder` and `delay`, a display string,
  and the `cut` capability. moq-transcode keeps what it needs through a
  `pub(crate)` path or a narrower accessor.
- `moq_video::encode::Config::preset` (`rs/moq-video/src/encode/encoder.rs:174`):
  crate-private, with `Preset` if nothing else names it. Outside the crate
  only `rs/moq-video/examples/encode-presets.rs` sets it and one
  moq-transcode test reads it. The audit's reviewer would have kept
  `Preset` as a small fenced enum; the maintainer chose to hide the set.
  [The OBS adapter](/quest/m3/obs-moq-video/adapter.md) is the planned
  consumer of both types and publishes them again, in the shape its Stats
  panel needs, which is additive on a `#[non_exhaustive]` config.
- `hang::health` (`rs/hang/src/health.rs`, 737 lines; `rs/hang/src/lib.rs:28`)
  and `Hang.Health` (`js/hang/src/health.ts`, 422 lines, 14 exports;
  `js/hang/src/index.ts:19`): private in both languages until
  [client health has a reader](/quest/m3/stats/health-consumers.md).
  No Rust crate names `hang::health`, and the JS export line is the only
  reference outside the module and its test. That quest re-measures every
  threshold and adds a per-connection counter the grade needs, and
  `Sample.interval`, `Sample.age`, and `Thresholds.stale` are still bare
  millisecond numbers, so the shape would change after publishing.
- Recommended, not on the maintainer's list: `moq_rtmp::PUBLISH_IDLE_TIMEOUT`
  (`rs/moq-rtmp/src/lib.rs:78`) has no user outside the crate.

JS:

- The `supported` input on the watch sources: remove
  `SourceInput.supported` on `Video.Source` and `Audio.Source`
  (`js/watch/src/video/source.ts:41`, `js/watch/src/audio/source.ts:30`),
  the `Video.Supported` and `Audio.Supported` types, the statics
  `Video.Decoder.supported` and `Audio.Decoder.supported`
  (`js/watch/src/video/decoder.ts:552`, `js/watch/src/audio/decoder.ts:1183`),
  and the `supportCacheKey` protocol in `js/watch/src/support-probe.ts`.
  The sources always use the decoder's probe. Every caller passes exactly
  that default: `js/watch/src/player.ts:159`, `:165`,
  `demo/vr/src/index.ts:11-12`, `demo/boy/src/game.ts:144`, `:151`. It is
  a callback parameter, which AGENTS.md asks to replace, and
  [the watch worker](/quest/m4/watch-worker.md) would have to carry it
  across a thread boundary. If a caller needs "can this browser play this
  config", keep one read-only helper and say so.
- `Report.INTERVAL` (`js/hang/src/report.ts:11`) is a bare `1000`; both
  callers wrap it in `Duration.millis` (`js/publish/src/broadcast.ts:409`,
  `js/watch/src/echo.ts:151`). Export a `Duration`, as
  `Container.DELAY_MAX` is, or stop exporting it.
- `SyncOutput.reference` (`js/watch/src/sync.ts:76`) is a public bare
  millisecond number read only by `js/watch/src/audio/buffer.ts:382`,
  `:515`: mark it `@internal` (`js/tsconfig.json` strips those).
- `BroadcastInput.tail` (`js/watch/src/broadcast.ts:245`) is a public input
  only `Player` can drive correctly: mark it `@internal`.

For the PR: delete rather than hide wherever nothing in the crate needs the
item. Each crate's and package's `MIGRATE.md` names what left, and
`doc/lib` drops what it documented. If one PR is too wide to review, split
Rust from JS; `Health` moves in both halves or neither.

Public API: moq-mux, moq-video, moq-transcode, hang, `@moq/hang`, and
`@moq/watch` lose the items above. Wire: none.

## Related

- [Client health in use](/quest/m3/stats/health-consumers.md) - publishes `Health` once something reads it
- [The OBS adapter](/quest/m3/obs-moq-video/adapter.md) - the planned consumer of `Preset` and `Applied`
