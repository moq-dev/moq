# [M] Measure and reduce fMP4 fragment encoding copies

## Goal

Reduce measured sample concatenation and fragment-header work without changing
encoded media semantics, public APIs, or the wire format.

## Plan

`container/fmp4/mod.rs` collects frame payloads into an mdat Vec, encodes moof
once to determine offsets, and encodes it again before copying mdat into the
output. Measure payload copies separately from metadata cloning and encoding.

Investigate sizing or reserving the header and copying sample payloads directly
into the final output. Preserve checked sizes and offsets, decode and presentation
timing, flags, duration handling, and existing errors. Avoid duplicating box
layout logic unless its maintenance cost is justified by the measurements.

Add Criterion cases with 1, 30, and 180 samples, audio and video, and differing
sample sizes. Wire round-trip semantic checks and overflow, duration, and offset
regressions into existing mux tests. Follow the measurement and no-win completion
rules in the [questline](/quest/m2/mux-copies/README.md).

## Related

- [fMP4 import](/quest/m2/mux-copies/fmp4.md) - separately shippable importer work
- [CMAF passthrough](/quest/m2/mux-copies/cmaf-passthrough.md) - may bypass this encoder for eligible HLS inputs
