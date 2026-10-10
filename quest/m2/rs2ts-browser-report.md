# [M] Compare generated lite in the browser

## Goal

A reproducible browser report compares generated lite with the hand-written
js/net it replaces on bundle size, per-frame CPU, and first-frame latency.
It records whether the generated implementation regresses, with retained
measurements and enough context to reproduce the comparison.

## Plan

Decided in the 2026-10-10 audit: move the final rs2ts comparison into m2 with
the browser benchmark harness. The translator's earlier bundle-size and
per-frame CPU go/no-go still gates the m1 implementation before the sans-IO
API changes. This report follows generated lite; it does not block it.

Compare identified generated and hand-written revisions through the browser
harness under the same workload and environment. Retain the baseline even
though generated lite deletes the hand-written implementation. Follow the
harness's controls for variability and record unsupported measurements
explicitly. Report any regression and the evidence needed to scope its fix;
do not assume that generated code is faster or substitute the codec-only
go/no-go for the browser measurements.

Public API: none. Wire: none.

## Required

- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - the real-browser harness and artifact conventions
- [Generated lite](/quest/m1/rs2ts/lite.md) - the implementation being compared

## Related

- [Generated @moq/net](/quest/m1/rs2ts/README.md) - the implementation line whose final comparison moved here
