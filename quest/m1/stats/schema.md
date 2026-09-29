# [M] hang defines the stats and echo sections and their snapshots

## Goal

`hang::Catalog` carries two optional root sections, `stats: { track }` and
`echo: { track }`. hang defines the snapshot types those tracks carry: the
publisher's per-track and transport counters, and a viewer's per-track and
transport feedback. Rust and JS parse the same fixtures, and the hang draft
specs both. Nothing produces them yet.

## Plan

- `rs/hang/src/catalog`: `Stats { track }` and `Echo { track }`, both
  `#[non_exhaustive]`, as `Option` fields on `Catalog` that are omitted from
  the wire when absent. Additive on main.
- `rs/hang/src/stats.rs`: the publisher snapshot,
  `Snapshot<E = ()> { transport, tracks: BTreeMap<String, Track>, #[serde(flatten)] ext: E }`.
  The generic lets moq-mux flatten `{ mpegts: ts::Stats }` in beside it, the
  way `Catalog<E>` takes `ts::Ext`. `Track` holds sent frames, sent bytes,
  keyframes, skipped frames, and the target bitrate as a gauge.
- `rs/hang/src/echo.rs`:
  `Snapshot { transport, tracks: BTreeMap<String, Track> }`, keyed by the
  soliciting publisher's track names. `Track` holds:
  - received frames and bytes, decoded, late, and decode errors;
  - stalls, stalled duration, and underruns;
  - the newest media timestamp received, with the wall time it arrived;
  - the playout latency, as a gauge.
- `Transport` is shared: rtt, estimated rate, bytes and packets lost, and
  sample age. Every field is optional, because a browser has only PROBE rtt.
- Every field is defaulted, zero and `None` are omitted, unknown fields are
  ignored, and each type is `#[non_exhaustive]`. Durations are milliseconds
  and rates bits per second, as in `moq-stats`.
- A helper names the `.echo` suffix (`hang::echo::is_echo(path)`), so a
  reader filters at announce time.
- `js/hang`: zod schemas mirroring both sections and all three snapshot
  types, field for field. Fixtures are shared through `js/test`.
- `drafts/draft-lcurley-moq-hang.md` specs the two sections, the snapshot
  schemas, the `.echo` convention, and its serving rule: a viewer
  accepts any requested track name, reports zeros until a watched catalog
  claims it, refuses a second claim on a bound name, and caps unclaimed
  names. Validate with `just drafts check`.
- Open, to settle before fixing the wire shape: a rendition may reference
  another broadcast, so one track name can name two renditions and a
  snapshot keyed by name collapses them. Candidates: key by the relative
  broadcast path and track name, or cover only the tracks in the
  publisher's own broadcast, where names are unique.
- Docs: `doc/concept/hang.md`, and a media section in `doc/concept/stats.md`.
- Tests:
  - fixtures round-trip in both languages;
  - an old catalog without the sections parses unchanged;
  - a snapshot with an unknown field parses.

## Related

- [#4145](https://github.com/moq-dev/moq/pull/4145) - the closed moq-stats
  extension design; salvage its field docs and serde helpers
