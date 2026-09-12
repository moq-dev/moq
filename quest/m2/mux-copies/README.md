# Mux and gateway copy budgets

## Goal

Import, export, and live HLS transmux copy coded bytes once (or not at
all when a `Bytes` view is already the hang frame). Each child lands a
Criterion target under `moq-mux` (or the gateway crate) plus an identity
check against the current output.

[CMAF copies](/quest/m2/cmaf-copy-budget.md) is the JS container. This
line is Rust.

## Quests

- [fMP4 import and fragment encode](/quest/m2/mux-copies/fmp4.md) - stop copying mdat two or three times
- [CMAF hang groups passthrough](/quest/m2/mux-copies/cmaf-passthrough.md) - live HLS does not remux samples it already has
- [Annex-B split](/quest/m2/mux-copies/annexb.md) - complete AUs are not copied per NAL
- [MPEG-TS ingest](/quest/m2/mux-copies/ts.md) - no 188-byte bounce plus drain-shift
- [FLV and RTMP tag bodies](/quest/m2/mux-copies/flv-rtmp.md) - one copy from tag buffer to hang payload
