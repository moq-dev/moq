# [L] HLS export hands a rendition over at a same-codec config change

## Goal

When a rendition changes resolution (or any other decoder-affecting field) while keeping its codec string, its HLS media playlist hands over to the new config without dropping or mislabelling a segment:

- The first group after the change is listed under the new init and segment tag, whichever of the catalog and the HLS records reaches the export first.
- The playlist keeps the old instance's segments already in its window, each under its own `EXT-X-MAP`, and lists the new config's behind an `EXT-X-DISCONTINUITY` (RFC 8216 6.2.2), so every served segment decodes under the init it is listed with and the rendition's recording cursor keeps up.

The conformance test in [moq-dev/moq#7387](https://github.com/moq-dev/moq/pull/7387) is the end-to-end regression test: its `video1` same-codec switch must pass.

## Plan

Found by the conformance test in [moq-dev/moq#7387](https://github.com/moq-dev/moq/pull/7387). The export already replaces the instance on a decoder-identity change (the same fields hang's `same_identity` compares, plus geometry). The rerun with a repaired fixture showed two hand-over faults instead:

- The new config's first group can reach the old instance's records watcher before the catalog update. The retiring instance claims it, so it is muxed under the old init and the replacement starts one group late.
- `Renditions::sync` dropped the retired instance from the playlist URI at once, and the replacement started with an empty window.

Decided 2026-10-31 (maintainer): re-scope this quest to the hand-over, resized [M] to [L]. The hand-over group must not depend on which track's data arrives first.

Done in [moq-dev/moq#7436](https://github.com/moq-dev/moq/pull/7436): the playlist keeps the retired instance's segments, under their own init and URLs, until the window slides past them, then lists the replacement's behind a discontinuity. Segment and init requests for the predecessor's URLs are served by it.

Remaining: the hand-over pick. The catalog and the records are separate tracks, so the export alone can't tell whether a record that arrived before the catalog update is the old config's or the new one's. The publisher has to mark it, which is a wire or publisher-behavior change; see the PR's `## Decisions needed`.

Public API: none. Wire: none so far; the pick may need one.

## Related

- [HLS and DASH segment conformance](/quest/m1/hls-conformance.md) - the test that found it, which waits on this
