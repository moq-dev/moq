# [M] Untrusted media parsers have fuzz targets

## Goal

The media parsers reachable from untrusted publishers (fMP4 import and CMAF
decode, the MKV, FLV, and TS importers, the Annex-B splitters, and hang
catalog parsing) run under nightly fuzz targets that assert bounded work
and allocation per input byte, not just "no panic".

## Plan

From the 2026-10-24 pre-cut audit at 151a529670. `rs/moq-mux` and `rs/hang`
have no fuzz targets; only `rs/moq-net/fuzz` and `rs/moq-quic/fuzz` exist,
and [wire fuzz coverage](/quest/m2/wire-fuzz-coverage.md) puts media
importers out of scope. The audit's
[zero-size sample](/quest/m1/cut-audit/fmp4-sample-resolver.md) and
malformed avc1 codec-string findings are the kind
`cargo fuzz` finds in minutes; they are reachable through RTMP, SRT, HLS,
and fMP4 ingest and through catalog subscription. Include RTMP's AMF0
decoder too, after [its budget fix](/quest/m1/cut-audit/rtmp-amf0-budget.md).

Add a fuzz crate per package, or one shared media target crate, seeded from
the existing fixtures. Assert a work and allocation bound proportional to
input size. Wire the targets into the nightly gate next to moq-net's, and
replay committed regressions in the merge gate.

Public API: none. Wire: none.
