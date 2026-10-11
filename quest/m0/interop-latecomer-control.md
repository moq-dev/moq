# [S] Interop's lagging latecomer control fails as designed

## Goal

Interop's "control: lagging latecomer" negative control reliably fails
"late join reaches live", so a passing positive case means something. A
failing Interop step no longer hides the steps after it: the TS steps run
and report on their own whatever the browser media step did.

## Plan

Found on #5207 (2026-10-10). The control (`test/interop/interop.sh`, added in
#5173) runs `test/interop/clients/js/media.ts --lag --cases late-join
--expect-fail "late join reaches live"`. In 3 of 4 runs on #5207, and on
unrelated PRs, the lagged latecomer still reached live, so the control
failed. Because the media step fails first, CI skips every TS Interop step,
so TS regressions can land with no CI signal (the `--headroom` arm added in
#5207 has not run in CI yet).

- Find why `--lag` doesn't keep the player behind `LIVE_LAG_FRAMES` (the
  player may catch up, the delay may not apply, or live detection may be too
  loose), and fix that at its cause. Never widen thresholds, add retries, or
  lengthen timeouts.
- Decided (2026-10-10): in this quest, make the Interop steps independent, so
  one red step never skips another. The job still fails if any step fails.

Public API: none. Wire: none.

## Related

- [TS duration fidelity](/quest/m0/ts-duration-fidelity.md) - the other failure that keeps Interop red on main
