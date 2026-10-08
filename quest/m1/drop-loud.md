# [S] Fail loud on dropped timed metadata

## Goal

Import stops dropping timed metadata silently. FLV script tags (RTMP and FLV
import) and fMP4 `emsg` boxes are counted and reported when discarded, so an
operator can tell that a source carried metadata MoQ did not deliver. Real
carriage stays with the m3 timed-metadata quests.

## Plan

Today `rs/moq-mux/src/container/flv/import.rs` matches `TAG_SCRIPT => {}`,
and the fMP4 importer's box loop (`rs/moq-mux/src/container/fmp4/import.rs`)
sends `emsg` to the catch-all arm that skips unknown atoms, both without a
log line. RTMP metadata never reaches the FLV importer: the publish side
discards `ServerSessionEvent::StreamMetadataChanged`
(`rs/moq-rtmp/src/server.rs`) and the pull side discards
`ClientSessionEvent::StreamMetadataReceived` (`rs/moq-rtmp/src/dial.rs`), so
those two arms are counted too.

Decided 2026-10-08, from the quest audit: a small m1 quest, since the
carriage quests are parked in m3 with no consumer and the silent drop
violates fail-loud in the meantime.

Recommendation: count and warn rather than refuse. Every RTMP publisher sends
`onMetaData`, so refusing a script tag would break every RTMP ingest. Keep a
per-importer count by kind (script tag, `emsg`), warn the first time each kind
drops on a broadcast with the count readable afterwards, and leave the debug
line for genuinely unknown tags. If a refusal fits a narrower case better
(for example an `emsg` scheme an application declared it needs), say so in
the PR.

Test: an FLV clip with `onMetaData`, an fMP4 fragment with an `emsg` box, and
an RTMP publish and pull carrying metadata each import, warn once, and report
a count of what was dropped.

Public API: possibly an accessor for the counts. Wire: none.

## Related

- [fMP4 emsg](/quest/m3/emsg.md) - carries the boxes this counts
- [FLV script tags](/quest/m3/flv-script.md) - carries the tags this counts
- [ID3 catalog section](/quest/m3/id3.md) - typed carriage for one `emsg` payload
- [CEA-608/708 extraction](/quest/m3/captions-cea.md) - the other timed-metadata carrier, kept in the SEI rather than dropped
