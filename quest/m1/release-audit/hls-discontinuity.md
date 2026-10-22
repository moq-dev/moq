# [M] Settle timestamp-reset HLS discontinuity behavior

## Goal

HLS import either supports a timestamp-reset discontinuity with correct
published identity or refuses it explicitly under a documented contract.
Never reuse a track name for a rewound timeline or silently rewrite media
timestamps to hide the problem.

## Plan

P2 interoperability investigation, source-traced rather than an end-to-end
reproduction. At main 6369eca, `rs/moq-hls/src/import.rs:590-599` handles a
changed discontinuity counter with `importer.discontinuity()` and `seek()`,
keeping its importer and track names when the initialization map is unchanged.
The fMP4 importer's discontinuity contract explicitly still rejects rewinds
(`rs/moq-mux/src/container/fmp4/import.rs:1364-1376`). A changed map instead
creates a new importer/track generation. Thus the same codec/map with a reset
`tfdt` has a credible unsupported path despite gateway discontinuity handling.

Reproduce unchanged-map resets and forward discontinuities across audio/video
renditions, and establish release provenance. Ask the maintainer whether to
support resets through appropriate track/timeline identity or explicitly
refuse this input with documented scope. Recommend preserving source media
times and the existing no-rewind invariant; do not assume timestamp rebasing
or a whole-broadcast epoch restart. Implement only the settled contract and
carry its playback/identity tests and HLS documentation inline. Report any
public API or wire impact; none is chosen by this investigation.

## Related

- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - decoder identity changes already mint new track names, but same-codec timestamp resets are not covered
