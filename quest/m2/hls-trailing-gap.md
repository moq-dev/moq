# [XS] An ended HLS playlist has no trailing gap

## Goal

A media playlist that ends with `EXT-X-ENDLIST` no longer lists a zero-duration `EXT-X-GAP` segment after its last real segment.

## Plan

Found by the conformance test in [moq-dev/moq#7387](https://github.com/moq-dev/moq/pull/7387), which allows the gap because the export writes it by design: a track's end writes a zero-duration stop record. Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Keep the stop record; omit it from the rendered playlist when `EXT-X-ENDLIST` follows, and drop the conformance test's allowance.

Public API: none. Wire: none.

## Related

- [HLS and DASH segment conformance](/quest/m1/hls-conformance.md) - the test that allows the gap today
