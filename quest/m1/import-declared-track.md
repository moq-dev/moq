# [S] Every importer announces only once it has a track

## Goal

Every `moq import` input follows one rule for when it has declared a track:
the broadcast is announced once its catalog names at least one servable track,
and an input that ends before then exits non-zero without announcing. A
servable track is a rendition, or an extension track such as `--passthrough`'s
`m2ts` multiplex, which publishes no rendition. Today a TS
input carrying only program tables can still announce an empty catalog at end
of input and exit 0.

The rule covers the stdin path, TS (including `--program all`), fMP4, FLV,
Annex-B/avc3, and HLS.

Non-goals, both declined in [#5517](https://github.com/moq-dev/moq/pull/5517):

- HLS holding the announce until every discovered rendition is ready. A dead
  variant is left out of the first catalog, as moq-hls intends.
- FLV waiting for every track type its header flags declare.

## Plan

[#5517](https://github.com/moq-dev/moq/pull/5517) made the stdin path wait for
the catalog's first snapshot and bail with "input ended before it declared a
track". Each importer still decides on its own when to release that snapshot:

- TS releases at its first PES with a PTS. The `Pending` doc in
  `rs/moq-cli/src/publish.rs` says the first PMT, so the doc and the code
  disagree.
- End of input releases every reservation. A program whose PMT never
  arrived is announced anyway.
- fMP4 releases at the moov, and FLV at its first media frame.

Recommended definition: a PMT or header entry alone does not count. A track
counts once the catalog names it: a rendition with a known codec config, or an
extension track. Settle it in one place that every importer shares, rather than per
importer. Fix the `Pending` doc to match.

Tests: a TS input with a PAT and PMT but no PES, an FLV with no frames, and
an avc3 input with no SPS each fail unannounced. Each importer's normal
fixture still announces, and so does `--passthrough`.

Update `doc/bin/cli.md` inline if it describes the exit behavior.

Public API: none expected. Wire: none.

## Related

- [Same-epoch importers](/quest/m2/hop-aligned-import.md) - same-epoch importers publish identical tracks
