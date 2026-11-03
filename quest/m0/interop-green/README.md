# [XS] Interop and Video pass on main

## Goal

The scheduled Interop and Video workflows pass on `main`, so a red run
means a new break. Interop has failed every scheduled run since
2026-10-22, and Video fails on a stale expectation.

## Waiting

- moq-dev/moq: a scheduled Interop run on `main` passes, once this quest's
  fixture fixes, wire-compat remeasure, and 90-minute job budget have
  landed. Check with
  `gh run list --repo moq-dev/moq --workflow interop.yml --event schedule`.
  Triage any red run, then delete this quest once one passes.

## Plan

Decided 2026-10-27 (maintainer), from an external release-to-main audit
and a triage of Interop runs
[40517499786](https://github.com/moq-dev/moq/actions/runs/40517499786),
[40344007093](https://github.com/moq-dev/moq/actions/runs/40344007093), and
[40195571889](https://github.com/moq-dev/moq/actions/runs/40195571889):
one child per distinct cause, each landing on its own, as the
[Nightly epic](/quest/m0/nightly/README.md) does.

Not failures: the `FAIL pinned round`, `timeout round`, `empty round`, and
port-reservation lines are negative self-tests in
`interop/relay-log.test.sh` and `lib/harness.test.sh` that pass.
Already owned: the intermittent media-matrix timestamp-gap cells
(`cpp -> js-native-node`, `go -> c`, browser `rust -> js` timeouts) belong to
[test flakes](/quest/m0/test-flakes-2/README.md), with
[moq-dev/moq#6990](https://github.com/moq-dev/moq/pull/6990) open. So does
the fallback audio stall, which runs
[41495461904](https://github.com/moq-dev/moq/actions/runs/41495461904) and
[41668668824](https://github.com/moq-dev/moq/actions/runs/41668668824)
hit on `moq-lite-07-wip`; see
[the IETF22 fallback quest](/quest/m0/test-flakes-2/ietf22-fallback.md).

Every child has landed. Video passed its scheduled
[run 41497602703](https://github.com/moq-dev/moq/actions/runs/41497602703).
Interop runs
[41317901057](https://github.com/moq-dev/moq/actions/runs/41317901057),
[41495461904](https://github.com/moq-dev/moq/actions/runs/41495461904), and
[41668668824](https://github.com/moq-dev/moq/actions/runs/41668668824)
showed more causes, fixed in this quest's own PR:

- The paused-stall browser tests time out because the stubbed watch in
  `test/interop/clients/js/buffering/main.ts` lacks
  `video.source.out.config`, which the buffering indicator reads since
  [moq-dev/moq#7155](https://github.com/moq-dev/moq/pull/7155). Its effect
  throws, so the spinner never hides.
- The Interop job took 39 to 60 minutes on recent runs, and the 60-minute
  `timeout-minutes` cancelled 41495461904 during its last test step.
- The open-GOP recovery browser test times out because the stubbed
  broadcast in `test/interop/clients/js/open-gop/main.ts` lacks `in.name`,
  which the video decoder reads for its stats key since
  [moq-dev/moq#7222](https://github.com/moq-dev/moq/pull/7222).
- Wire-compat fails on a stale planned break: the `moq-lite-07-wip` entry
  in `test/interop/compat/planned-breaks.json` pins moq-cli 0.14.10, and
  0.14.11 has been released. Remeasured against moq-cli 0.14.12 and
  moq-relay 0.17.12 with `just test wire-compat --remeasure`.

The P2P data-channel test timed out once, in 41668668824, and passes
locally; triage it if it recurs.

Public API: none. Wire: none.
