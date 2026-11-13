# Interop and Video pass on main

## Goal

The scheduled Interop and Video workflows pass on `main`, so a red run
means a new break. Interop has failed every scheduled run since
2026-10-22. Video has passed every scheduled run since 2026-11-02 (for
example [42063654592](https://github.com/moq-dev/moq/actions/runs/42063654592)),
so it only needs to stay green.

## Waiting

- moq-dev/moq: the next scheduled Interop run on `main` passes, once the
  [resumed-audio child](/quest/m0/interop-green/resume-sync.md) lands
  ([moq-dev/moq#8572](https://github.com/moq-dev/moq/pull/8572), the max-age
  fix, merged 2026-11-12). Check
  with `gh run list --repo moq-dev/moq --workflow interop.yml --event schedule`,
  then the run's STEPS summary. The JS route port that [moq-dev/moq#8290](https://github.com/moq-dev/moq/pull/8290)
  waited on landed in [moq-dev/moq#8443](https://github.com/moq-dev/moq/pull/8443),
  so the pass now runs the datagram step; the `replacement` case stays
  quarantined by default. Decided 2026-11-09 (maintainer, AFK, accepting the
  recommendation): close this quest on a pass, which the maintainer set with
  those cases skipped; the pass that closes it now runs them. Decided
  2026-11-12 (maintainer): this epic also waits for its
  [planned-break remeasure](/quest/m0/interop-green/planned-break-remeasure.md)
  child, so the pass closes the epic only once that child lands too. Until then,
  a red run whose only failure is a stale planned break is fixed by hand: run
  `just test wire-compat --remeasure` and land the rewritten
  `test/interop/compat/planned-breaks.json`, as RELEASE.md's release
  procedure describes.

## Plan

Decided 2026-11-12 (maintainer), since green scheduled Interop is this epic's goal and stale pins caused four of the last six red runs:

- ✅ the release-remeasure pipeline is a child quest of quest/m0/interop-green/ (rejected: quest/m3/release/; drop the follow-up). See [planned-break remeasure](/quest/m0/interop-green/planned-break-remeasure.md).
- ✅ the release cut keeps its manual post-publish `just test wire-compat --remeasure` step until that child lands, and does not require it (rejected: block the cut on the new quest; accept a red Interop run per release).

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

Scheduled run
[42382888974](https://github.com/moq-dev/moq/actions/runs/42382888974)
(2026-11-07, at b98833c40a, after those fixes landed) failed only Harness
regressions: `publish-ui.browser.ts`'s invalid-attribute check still tested
moq-watch's `buffer`, which
[moq-dev/moq#8057](https://github.com/moq-dev/moq/pull/8057) renamed to
`lookahead`. Fixed in this quest's PR: the case tests `lookahead`, and a new
check confirms `buffer` reports its rename and recovers once removed.

Scheduled run
[42491651687](https://github.com/moq-dev/moq/actions/runs/42491651687)
(2026-11-08, at e2271bad9c) failed two steps, and neither is a flake. Both come
from the JS lite-07 route wire, which Rust moved to but JS has not ported, so
they were owned by the JS route port, which landed as
[moq-dev/moq#8443](https://github.com/moq-dev/moq/pull/8443), not this quest:

- Datagram tracks: `datagram_tracks_javascript` times out in its first case
  (relay false, JS publishes false). It fails every time on main's content
  (`just test datagram-tracks`), not under load.
- Media output and lifecycle: chromium's `replacement` case crashes both
  moq-lite-07-wip pages with `page crashed`. It fails every time on main's
  content (`just test media --media-cases replacement`), though it passed on an
  older base.

[moq-dev/moq#8290](https://github.com/moq-dev/moq/pull/8290) skipped the
datagram step and quarantined `replacement` by default, which this quest's
triage confirms. On main's content, `just test media` passes; the datagram test
still fails until the port lands. No new quest or retry is needed here.

Scheduled run
[42673842120](https://github.com/moq-dev/moq/actions/runs/42673842120)
(2026-11-09) and
[42851744235](https://github.com/moq-dev/moq/actions/runs/42851744235)
(2026-11-10) failed Harness regressions and Released wire compatibility. The
staging fixture's fake `cargo` lacked `tree`, which `test/interop/compat.sh`
calls, so its test exited 2; [moq-dev/moq#8455](https://github.com/moq-dev/moq/pull/8455)
stubs it. The planned break's pin, moq-cli 0.14.14, went stale when 0.14.15
shipped.

Scheduled run
[43052960736](https://github.com/moq-dev/moq/actions/runs/43052960736)
(2026-11-11, at a4bc922c6) failed the same two steps. The invalid-attribute
browser case restored `lookahead` to the number `0` after its `try` block,
which the setter refuses, so the evaluate threw.
[moq-dev/moq#8512](https://github.com/moq-dev/moq/pull/8512) passes the page's
own Duration and remeasures the pin to moq-cli 0.14.16.

`main` at d3b6c2573 has both fixes, and every pin matches the latest release of
its package (moq-cli 0.14.16, moq-relay 0.17.16, @moq/net 0.4.7). The stale pin
is the recurring cause: each published release of a pinned package turns the
next scheduled run red until someone remeasures.

Scheduled run
[43260674067](https://github.com/moq-dev/moq/actions/runs/43260674067)
(2026-11-12, at e9c6df852) failed three steps, each with its own cause:

- Optional publisher retention: `max_age_relay_javascript` fails on lite-07
  track 1, which reads a max age of 0 where the test expects none.
  [moq-dev/moq#8443](https://github.com/moq-dev/moq/pull/8443) added
  `moq-lite-07-wip` back to `max_age_relay_javascript` in `max_age.rs`, but the
  JS subscriber kept its moq-transport-only expectation of no max age; lite
  carries it, as the Rust side of the test expects.
  [moq-dev/moq#8572](https://github.com/moq-dev/moq/pull/8572) fixes the
  expectation, and is open.
- Interop (full matrix): `go -> python` failed with `audio gap after 648 samples`
  in the scheduled run. [moq-dev/moq#8633](https://github.com/moq-dev/moq/pull/8633)
  could not reproduce it in ten completed full matrices on 2026-11-13: the cell
  passed all ten, nine matrices passed entirely, and one failed other cells.
  No runtime fix is claimed. The related
  [moq-dev/moq#8611](https://github.com/moq-dev/moq/pull/8611) reproduces a
  missing-group skip under CPU starvation in the same consumer and owns that fix.
- Media output and lifecycle: after the pause case resumes, the audio trails the
  picture by about 1.7 s. [moq-dev/moq#8507](https://github.com/moq-dev/moq/pull/8507)
  keeps audio that trails the clock up to its 2 s stretch bound and plays it
  faster, and a resume keeps the ring it had. Fixed by the child
  [resumed audio snaps to the clock](/quest/m0/interop-green/resume-sync.md),
  decided 2026-11-12 (maintainer) to snap to the live picture rather than
  stretch the lag back.

The 11-11 run, before #8507, passed all three steps.

Public API: none. Wire: none.

