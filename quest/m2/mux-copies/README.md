# Mux and gateway copy budgets

## Goal

Measure Rust container and gateway payload copying, then remove redundant work
where the improvement survives representative benchmarks. Each child ships
independently without changing public APIs, wire formats, or supported input.

## Plan

Count copied payload bytes, header and metadata work, allocations, and retained
buffer memory separately. A `Bytes` clone or slice is not a payload copy. Report
CPU and allocation results for identical fixtures and caller chunk sizes before
and after a change; do not promise a universal copy count or speedup in advance.

Each child adds representative Criterion coverage discoverable by `just bench`
and correctness fixtures wired into existing CI tests. Use the Nix shell and
`just bench <base>` for comparison, keeping the same benchmark cases available
on both measured implementations. Check A/A noise and an intentionally redundant
copy variant so the benchmark can detect the proposed improvement. A new case
without a base sample is not evidence of a speedup.

A child may complete with retained benchmarks and documented findings if no
repeatable improvement justifies the added complexity. Keep an optimization only
when it improves the measured workload without a material regression in other
covered shapes or retained memory. These are implementation optimizations;
public API or wire changes require separate planning.

## Quests

- [fMP4 import](/quest/m2/mux-copies/fmp4.md) - measure and reduce mdat extraction copies
- [fMP4 fragment encoding](/quest/m2/mux-copies/fmp4-encode.md) - measure and reduce sample concatenation and header work
- [CMAF hang groups passthrough](/quest/m2/mux-copies/cmaf-passthrough.md) - avoid measured HLS sample remux work where semantics match
- [Annex-B split](/quest/m2/mux-copies/annexb.md) - measure complete access-unit ingestion and assembly
- [MPEG-TS ingest](/quest/m2/mux-copies/ts.md) - measure packet-buffer and reader-adapter copies
- [FLV tag bodies](/quest/m2/mux-copies/flv-rtmp.md) - reduce duplicate tag-body and media-payload copies
- [RTMP chunk assembly](/quest/m2/mux-copies/rtmp.md) - reduce unnecessary assembly of complete messages

## Related

- [CMAF copies](/quest/m2/cmaf-copy-budget.md) - browser container ownership and copying
