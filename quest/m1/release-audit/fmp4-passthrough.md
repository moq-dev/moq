# [M] Preserve separated sample runs in fMP4 passthrough

## Goal

fMP4 passthrough emits the original sample bytes and timing when a fragment
contains multiple runs separated by padding or another track's data. Rewritten
run offsets must describe the payload actually emitted.

## Plan

P1 data corruption for affected passthrough inputs, inherited from release.
At main 6369eca, `rs/moq-mux/src/container/fmp4/import.rs::Fmp4Track::run`
resolves each sample range but also combines the runs into `start..offset`.
`fragment` rewrites `trun` offsets as contiguous cumulative sample sizes,
then copies the entire aggregate range, including gaps.

A reproduction extracted the actual `fragment` function unchanged and linked
the repository's mp4-atom/bytes libraries. Two one-sample runs reference ranges
`0..2` and `4..6` of `AAxxBB`. Supplying the aggregate range computed by `run`
and decoding the output produced samples `AA, xx`, rather than `AA, BB`.
This verifies the remux component, not an end-to-end importer fixture.

Default import now unwraps individual samples, avoiding this path, but
passthrough remains supported by public `Import::with_passthrough`, CLI
`import fmp4 --passthrough`, and HLS `--passthrough`. The release implementation
has the same aggregate-span/cumulative-offset mismatch.

Build payload and rewritten offsets from the same resolved sample ranges.
Cover gaps, interleaved tracks, explicit offsets, successive implicit offsets,
and sample defaults in an importer-level roundtrip asserting bytes and timing.
Measure copy cost across tracks and runs so correcting the output does not
introduce repeated whole-payload copying. No public API or format-contract
change is expected; the emitted CMAF becomes correct. Land on main first and
propose a release-compatible backport for existing passthrough users.
