# [M] Live TS clock-rate validation identifies and fixes timing defects

## Goal

Resolve the local live PCR-rate failure seen during automated TS validation.
The exporter and the grader agree on the supported release-clock contract,
with reproducible evidence and automated controls that distinguish valid
output from actual timing defects.

## Plan

Decided with the maintainer on 2026-10-11: investigate and fix this finding
independently of the same-instance resume failure. Preserve strict publisher
identity, the existing validation gates, and production negotiation defaults.
Keep validation PR #5328 draft while its local readiness findings remain
unresolved; a passing CI run alone does not explain the local failure.

The epoch-capable 120-second local run completed with publisher and exporter
exit zero, but the grader reported -1382.60 ppm against its +/-30 ppm limit.
Release timing passed with 224.4 ms accumulated lag and -0.003 ms/s over the
last 38.7 seconds; byte-position scheduling passed at 100%. Logs recorded
WebSocket fallback, old-session forced closure, and a later video deadline
miss. These are observations, not an established causal chain. The same-head
full CI run passed with -0.24 ppm and no release-interval violations.

`test/ts/pcr-timing.py` already excludes the first third before fitting PCR
against arrival time. Do not attribute the failure to startup merely because
the tail is nearly flat. The retained local report contains aggregates, not
the raw arrival samples needed to replay that fit. Retain enough clock,
packet-position, discontinuity and arrival evidence to reproduce the verdict
and separate source timing, clock recovery, export release, pipe backpressure
and measurement behavior. Compare a direct connection with controlled
fallback/handover where useful; do not infer transport causality from timing.

Reproduce the cause before changing production code or the oracle. Correct
the layer that is wrong, including the grader if its measurement does not
support its claim. Never obtain a pass by widening the limit, choosing a
convenient measurement window, disabling fallback, or retrying. A necessary
change to the externally promised timing contract needs a maintainer decision.
Check current main and the related open work before duplicating a fix.

Add deterministic positive and negative timing controls with the intended
failure reason asserted. They should separate valid bounded delay and
measurement effects from the clock-rate or release defect being fixed.
Replay evidence where possible and mock time in unit tests. Keep byte-position
PCR accuracy distinct from arrival-clock measurements. Run short controls in
PR CI and real live timing at least nightly, retaining diagnostic artifacts.
Completion requires a causal regression and the corrected live validation,
not a single successful rerun. Update the harness documentation inline;
public API and wire changes are not expected.

## Related

- [Validation PR #5328](https://github.com/moq-dev/moq/pull/5328#issuecomment-6114456593) - retained local failure and process-status evidence
- [Passing full CI](https://github.com/moq-dev/moq/actions/runs/38178905198) - same-head contrast, including real live timing
- [Same-instance resume](/quest/m1/ts-same-instance-resume.md) - independent full-check failure
- [Transport upgrade](/quest/m1/transport-upgrade/README.md) - existing session handover and epoch contract
- [Mux-rate hold #5287](https://github.com/moq-dev/moq/pull/5287) - initial rate publication, and a finished import lingering for its max-age
- [TS AC-3 scheduling](/quest/m1/ts-atsc-ac3.md) - #5306 owns decoder deadlines and unknown-rate startup overflow
