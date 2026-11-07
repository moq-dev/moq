# [XS] Interop and Video pass on main

## Goal

The scheduled Interop and Video workflows pass on `main`, so a red run
means a new break. Interop has failed every scheduled run since
2026-10-22. Video has passed every scheduled run since 2026-11-02 (for
example [42063654592](https://github.com/moq-dev/moq/actions/runs/42063654592)),
so it only needs to stay green.

## Waiting

- moq-dev/moq: a scheduled Interop run on `main` passes. Check with
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
[test flakes](/quest/m0/test-flakes-2/README.md). The fallback audio stall
that hit on `moq-lite-07-wip` in
[41495461904](https://github.com/moq-dev/moq/actions/runs/41495461904) and
[41668668824](https://github.com/moq-dev/moq/actions/runs/41668668824) was a
replay of retained audio past the viewer's max delay, fixed in
[moq-dev/moq#7656](https://github.com/moq-dev/moq/pull/7656).

Video passed its scheduled
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

The P2P data-channel test timed out in 41668668824 and again in
[42062226837](https://github.com/moq-dev/moq/actions/runs/42062226837)
(2026-11-05): Chromium dropped the accepting side's first record, sent before
its SCTP stream opened. The accepting side now waits to hear the dialer first.

Scheduled run
[41863118968](https://github.com/moq-dev/moq/actions/runs/41863118968)
(2026-11-04, after every fix above landed) passed the open-GOP recovery
case, P2P, and wire-compat. Its only failure was the fallback audio stall
above, on `moq-lite-07-wip` in Chromium, which the IETF22 fallback quest
owns. Scheduled run 42062226837 (2026-11-05) failed only P2P and the same
fallback audio stall in Chromium (`audioStalled:true`).

Scheduled run
[42245660435](https://github.com/moq-dev/moq/actions/runs/42245660435)
(2026-11-06, at 01d4edbe59, before the P2P fix
[moq-dev/moq#8001](https://github.com/moq-dev/moq/pull/8001) merged) had no
media, P2P, or fallback failure. It failed three deterministic steps, fixed in
this quest's PR:

- Harness regressions: `staging.test.py` stubbed only the examples
  `interop.sh` runs itself, but `interop-socket-js` is built there and run
  only by `socket.sh` since
  [moq-dev/moq#7884](https://github.com/moq-dev/moq/pull/7884). The fixture
  now stubs every example in the build list.
- Subscription termination: `just test bare-fin` built moq-net's lib tests
  without the `async` feature its tests need, a configuration no other job
  compiles, so dead code under it broke the build. The recipe now enables
  `async`.
- Wire-compat: moq-cli 0.14.13 and moq-relay 0.17.13 made the
  `moq-lite-07-wip` planned break stale again; remeasured, with the same 16
  cells failing.

Public API: none. Wire: none.
