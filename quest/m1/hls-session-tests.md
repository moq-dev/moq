# [M] HLS export tests over a session

## Goal

A `rs/moq-hls/tests` integration suite records and serves a broadcast whose
publisher sits on the far side of a real moq-net session, and fails when the
export's end-of-recording contract breaks. The suite uses only moq-hls's public
API.

Each scenario checks both the recording cursor (`segments::Consumer::next`) and
the served playlists:

- **Publisher disconnect**: the publisher's session drops mid-segment or
  mid-catalog. The cursor drains the completed tail, then fails every call with
  a typed `TimelineFailed`, `CatalogFailed`, or `SegmentUnavailable`, whichever
  failure lands first. It never returns a raw transport error such as
  `Unroutable` or `Session(Cancel)`. The media playlist refuses rather than
  ending with `EXT-X-ENDLIST`.
- **Clean finish**: the catalog and tracks finish, with an announce End both
  before and after catalog EOF. The cursor reads to `None`, and the playlist
  carries `EXT-X-ENDLIST`.
- **Restart and takeover**: a new instance takes over the path. The old run's
  cursor ends, the new run serves under its own `@{instance}` and generation,
  and an old run's URL never serves the new run's bytes.

Out of scope:

- HLS import.
- A relay hop between the publisher and the export. The CLI recorder covers
  that path end to end.
- Wall-clock or socket transports.

## Plan

Decided 2026-10-25 (maintainer interview):

- **Why**: today's cursor failure tests inject errors into local producers
  (`cut()`, `finish(Err(..))`). Only `live_fmp4_segments` and a few
  `server/routes.rs` tests cross a session at all. A publisher that really
  disconnected, failing the catalog before the timeline, returned the raw
  error until [#6694](https://github.com/moq-dev/moq/pull/6694). Only a downstream embedder's end-to-end run caught it.
- **Shape**: integration tests in `rs/moq-hls/tests`, so an API shape an
  embedder cannot use fails here too. Promote what the suite needs from the
  `LitePair` helper in `server/routes.rs` rather than duplicating it.
- **Transport**: `moq_net::transport::memory::pair` with a `moq_net` `Client`
  and `Server`, under `moq_clock`'s simulated clock, as `live_fmp4_segments`
  does. A disconnect is dropping a side. No sleeps, no wall clock.
- **Versions**: the default moq-lite version only.
- **Unit tests stay**: the local-producer tests in `export/mod.rs` remain. They
  pin the cursor's logic, and this suite pins the session behavior.
- Each scenario must fail on a tree that reintroduces the bug it guards. For
  the disconnect race, revert [#6694](https://github.com/moq-dev/moq/pull/6694)'s wrapping and confirm the suite fails.

## Related

- [HLS recorder in the CLI](/quest/m1/hls-record-cli.md) - the cursors' first consumer outside moq-hls, and the end-to-end path with a relay hop
- [HLS restart drain cutoff](/quest/m2/hls-restart-drain-cutoff.md) - changes what a replaced run's cursor drains; update the takeover scenario with it
- [Test coverage audit](/quest/m2/audit-test-coverage.md) - the recurring audit this gap slipped past
