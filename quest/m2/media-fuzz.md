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
[zero-size sample](https://github.com/moq-dev/moq/pull/6864) and
malformed avc1 codec-string findings are the kind
`cargo fuzz` finds in minutes; they are reachable through RTMP, SRT, HLS,
and fMP4 ingest and through catalog subscription. Include RTMP's AMF0
decoder too; its borrowed receive views should stay allocation-free.

Add a fuzz crate per package, or one shared media target crate, seeded from
the existing fixtures. Assert a work and allocation bound proportional to
input size. Wire the targets into the nightly gate next to moq-net's, and
replay committed regressions in the merge gate.

Measure parse amplification too (decided 2026-10-31, maintainer, from
[moq-dev/moq#7232](https://github.com/moq-dev/moq/pull/7232)). That PR bounds
a held `moov` or `moof` at 32 MiB (`fmp4::MAX_HELD_BYTES`) and a `moof` at
2^21 samples (`fmp4::MAX_FRAGMENT_SAMPLES`), but a body at the bound
decodes into larger structures: an explicit `trun` entry costs 4 to 16
input bytes and about 40 parsed, so 2.5x to 10x. A `trun` without
per-sample fields parses to no entries, but planning still resolves every
sample it declares, up to the 2^21 bound. Record the peak allocation for a
maximal `moov`, a maximal `moof` of 4-byte entries, and a field-less `moof`
at the sample bound, and fix each or accept it with the number.

Public API: none. Wire: none.
