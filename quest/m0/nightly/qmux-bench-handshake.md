# [S] Nightly's WebSocket fallback benchmark connects again

## Goal

The scheduled Nightly's Audio quality job passes its "WebSocket fallback
startup benchmark" step (`just js qmux-bench`) on `main`, fixed at the cause
of its handshake timeout, so the job fails only on real audio results.

## Plan

Planned 2026-11-09 (maintainer, /quest-unblock). The step passed on the
2026-11-05 and 2026-11-06 scheduled Nightlies, then failed on 2026-11-07,
11-08 and 11-09 ([run 42659882487](https://github.com/moq-dev/moq/actions/runs/42659882487))
with `error: evaluate: Error: handshake timeout` from the Playwright
evaluate in `qmux-bench`. That's a regression, not a flake. The window is
[fbef872b](https://github.com/moq-dev/moq/commit/fbef872bdc7f2ec619ae515626d6cd07d3926445)..[b5805859](https://github.com/moq-dev/moq/commit/b5805859aa6bf856c1a96ab3af89d45757de8e3d)
(120 commits). Likely suspects, to check first:

- [moq-dev/moq#8054](https://github.com/moq-dev/moq/pull/8054): QMux sessions return once established.
- [moq-dev/moq#8021](https://github.com/moq-dev/moq/pull/8021): send-rate estimates sampled from one page-wide ticker.
- [moq-dev/moq#8078](https://github.com/moq-dev/moq/pull/8078): a bun test fails when its own timer fires on the host clock.

Reproduce locally with `just js qmux-bench`, bisect the window if no suspect
explains it, and list the culprit under the fix PR's `## Caused by`.

Public API: none expected. Wire: none expected.
