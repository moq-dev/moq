# [M] hang defines the stats and echo sections and their snapshots

## Goal

`hang::Catalog` carries two optional root sections, `stats: { track }` and
`echo: { path }`. hang defines the snapshot types those tracks carry: the
publisher's per-rendition and transport counters, and a viewer's
per-rendition and transport feedback. Rust and JS parse the same fixtures,
and the hang draft specs both. Nothing produces them yet.

## Plan

- `rs/hang/src/catalog`: `Stats { track }` and `Echo { path }`, both
  `#[non_exhaustive]`, as `Option` fields on `Catalog` that are omitted from
  the wire when absent. `path` is relative to the broadcast serving the
  catalog and resolves through `try_resolve`, like a rendition's
  `broadcast`; `<name>.echo` is appended to the result. A path that escapes
  the root refuses the catalog. Shared fixtures in `rs/hang/fixtures` (beside
  `catalog-clock.json`, which JS mirrors) pin both in both languages.
  Additive on main.
- `rs/hang/src/stats.rs`: the publisher snapshot,
  `Snapshot<E = ()> { transport, renditions: BTreeMap<String, Track>, #[serde(flatten)] ext: E }`.
  The generic lets moq-mux flatten `{ mpegts: ts::stats::Snapshot }` in beside it, the
  way `Catalog<E>` takes `ts::Ext`. `Track` holds sent frames, sent bytes,
  keyframes, skipped frames, and the target bitrate as a gauge.
- `rs/hang/src/echo.rs`:
  `Snapshot<E = ()> { transport, renditions: BTreeMap<String, Track>, #[serde(flatten)] ext: E }`,
  the same flatten as `stats::Snapshot`, so a TS export folds its
  `mpegts` egress counters in (decided 2026-10-12, from
  [#5360](https://github.com/moq-dev/moq/issues/5360)). `Track`
  holds:
  - received frames and bytes, decoded, late, and decode errors;
  - stalls, stalled duration, and underruns;
  - the newest media timestamp received, with the wall time it arrived;
  - the playout latency, as a gauge.
- Both snapshots key by the catalog's rendition ID. Feedback covers a
  rendition that references another broadcast under its ID in the catalog
  that lists it; the publisher snapshot omits referenced renditions (decided
  in the [README](/quest/m1/stats/README.md)).
- `Transport` is shared: rtt, estimated rate, cumulative bytes and packets
  lost, and sample age. Decided in the 2026-10-10 audit: loss is cumulative
  counters, not a reported gauge, so health derives loss over the same
  interval as rendition rates and handles resets. RTT and estimated rate
  remain gauges. Every field is optional, because a browser has only PROBE rtt.
- Every field is defaulted, zero and `None` are omitted, unknown fields are
  ignored, and each type is `#[non_exhaustive]`. Durations are milliseconds
  and rates bits per second, as in `moq-stats`.
- hang names the `.echo` suffix and the feedback track (a helper such as
  `hang::echo::is_echo(path)`), so a reader filters at announce time and a
  publisher subscribes by a known name.
- `js/hang`: zod schemas mirroring both sections and all three snapshot
  types, field for field. Fixtures are shared through `js/test`.
- `drafts/draft-lcurley-moq-hang.md` specs the two sections, the snapshot
  schemas, and the `.echo` convention: one broadcast per soliciting catalog,
  under the catalog's echo path, announced after the viewer reads the
  catalog. Validate with `just drafts check`.
- **MSF carries neither section yet** (decided 2026-10-12). The MSF
  conversion in `rs/moq-mux` copies only the media sections and `ext`. A
  viewer given `--echo` with `--catalog-format msf` refuses, since it would
  never see an `echo` section to answer. An import has no catalog-format
  flag and always emits the MSF catalog beside the hang one, so that catalog
  omits `stats` and `echo` the way it omits `json` and `binary`; hang
  readers still get them. Carrying them through MSF `publishTracks` is
  [MSF feedback tracks](/quest/m3/msf-publish-tracks.md).
- Docs: `doc/concept/hang.md`, and a media section in `doc/concept/stats.md`.
- Tests:
  - fixtures round-trip in both languages;
  - an old catalog without the sections parses unchanged;
  - a snapshot with an unknown field parses.

## Required

- [Catalog rendition IDs](/quest/m1/catalog-track-id.md) - the rendition
  ID both snapshots key by

## Related

- [#4145](https://github.com/moq-dev/moq/pull/4145) - the closed moq-stats
  extension design; salvage its field docs and serde helpers
