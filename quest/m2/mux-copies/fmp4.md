# [M] Measure and reduce fMP4 import copies

## Goal

Reduce measured payload copying during fMP4 import while preserving published
CMAF fragments' media, groups, timing, catalog, and input validation. Keep the
public API and wire format unchanged.

## Plan

`container/fmp4/import.rs` decodes an owned `Any::Mdat`, retains a view of the
consumed input, copies each selected track range into `Mdat::data`, and encodes
that data into the output fragment. Measure these distinct ownership steps.

Investigate reading mdat ranges from shared input and writing selected payloads
directly into the final fragment. Do not assume header rewriting can share a
single contiguous output without copying. Preserve box-boundary validation,
track selection, multi-traf/trun offsets, sample flags, durations, composition
offsets, native timescales, and existing malformed-input errors. Check retained
input-buffer memory as well as allocation counts.

Add Criterion import cases using
`rs/moq-mux/src/container/fmp4/test_data/bbb.mp4` plus generated multi-traf/trun
and fragmented-input fixtures. Wire semantic comparison and malformed-input
regressions into the existing mux tests. Follow the measurement and no-win
completion rules in the [questline](/quest/m2/mux-copies/README.md).

## Related

- [Fragment encoding](/quest/m2/mux-copies/fmp4-encode.md) - separately shippable exporter work
- [CMAF passthrough](/quest/m2/mux-copies/cmaf-passthrough.md) - HLS consumer of imported fragments
