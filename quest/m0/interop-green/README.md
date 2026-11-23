# [XS] Interop and Video pass on main

## Goal

The scheduled Interop and Video workflows pass on `main`, so a red run
means a new break. Interop has failed every scheduled run since
2026-10-22. Video has passed every scheduled run since 2026-11-02 (for
example [42063654592](https://github.com/moq-dev/moq/actions/runs/42063654592)),
so it only needs to stay green.

## Waiting

- moq-dev/moq: the next scheduled Interop run on `main` passes, now that
  [moq-dev/moq#8572](https://github.com/moq-dev/moq/pull/8572) (max age,
  2026-11-12) and [moq-dev/moq#8635](https://github.com/moq-dev/moq/pull/8635)
  (resumed audio, 2026-11-14) have landed. Check
  with `gh run list --repo moq-dev/moq --workflow interop.yml --event schedule`,
  then the run's STEPS summary. The JS route port that [moq-dev/moq#8290](https://github.com/moq-dev/moq/pull/8290)
  waited on landed in [moq-dev/moq#8443](https://github.com/moq-dev/moq/pull/8443),
  so the pass now runs the datagram step; the `replacement` case runs by
  default on both engines and every wire. Decided 2026-11-09 (maintainer, AFK, accepting the
  recommendation): close this quest on a pass, which the maintainer set with
  those cases skipped; the pass that closes it runs all but `replacement`.
  Release remeasure measures stale planned-break pins after publication and
  opens the pin PR on its `release-remeasure` branch.

## Plan

Found 2026-11-19 by the Nightly triage of run
[44562804083](https://github.com/moq-dev/moq/actions/runs/44562804083):
`Media (firefox player)` fails the `replacement` case on `moq-lite-07-wip`
(`FAIL live replacement plays: waiting for the new run's picture and
synchronized tone within 1039ms ... "audioStalled":true,"rms":0`). It was the
first night the case ran on Firefox, since on 2026-11-17 and 2026-11-18 the
job failed earlier on "after resume", which
[moq-dev/moq#9141](https://github.com/moq-dev/moq/pull/9141) fixed. This
epic owns the replacement case's history.

Decided 2026-11-20 (maintainer, /quest-plan), from the triage of Nightly run
[44767045952](https://github.com/moq-dev/moq/actions/runs/44767045952) and
scheduled Interop: two children,
the replacement join (that stall, which also reproduces on Chromium
`moq-transport-22`; [moq-dev/moq#9293](https://github.com/moq-dev/moq/pull/9293)
joins live and lifts the quarantine, leaving Chromium's moq-transport audio
stall, which [moq-dev/moq#9300](https://github.com/moq-dev/moq/pull/9300)
fixed: a relay could serve a group before the skip its header passed over,
so the group went out without its Prior Group ID Gap)
and the moq-transport JS joining FETCH, which the relay refused when the join's Largest
Object was a group's END_OF_GROUP status (fixed in
[moq-dev/moq#9291](https://github.com/moq-dev/moq/pull/9291)).
The intermittent `go -> js` silent audio went to
test flakes (fixed by the Go tone's start-anchored pacing), and the datagram
cells are [moq-dev/moq#9197](https://github.com/moq-dev/moq/pull/9197)'s.

Scheduled runs 2026-11-15 to 2026-11-21 (every one red) failed three causes, none a flake:

- Harness regressions: `open-gop.browser.ts` since
  [moq-dev/moq#9293](https://github.com/moq-dev/moq/pull/9293) made the player join
  live. [moq-dev/moq#9306](https://github.com/moq-dev/moq/pull/9306) fixed the fixture
  (merged 2026-11-22).
- Interop (moq-transport): `datagram rust -> js-native-node` and `-bun` on moq-transport-18
  fail every run, since [moq-dev/moq#8918](https://github.com/moq-dev/moq/pull/8918) gave
  every JS datagram a timestamp. [moq-dev/moq#9197](https://github.com/moq-dev/moq/pull/9197)
  fixed the assertion (merged 2026-11-22).
- Interop (full matrix and moq-transport): `js -> js`, `js -> js-native-*`, and `go -> js`
  timed out waiting for the catalog on moq-transport 18, 20, and 22, one to three cells per
  pass, so a fresh retry always hid it. Root cause: a relay holding a group with its
  END_OF_GROUP reports Largest at that status, which is no frame (and keeps doing so). The
  next subscriber's head fill, cut one past that Largest, ran off the end of the group, so
  the relay reset it NotFound, and the subscriber dropped the catalog group as headless.
  Fixed in this quest's PR: a fill cut past the terminal object completes at it. A relay
  copy resubscribing hit the same status as MalformedTrack in `judge`, which now compares
  its answer with the last data object. It first showed in the 2026-11-15 run; the culprit
  PR is not pinned, since bisecting a race that fails 0 to 2 of 6 cells locally found no
  clean split.

The 2026-11-22 scheduled run
[45041226494](https://github.com/moq-dev/moq/actions/runs/45041226494), at 040a566d40 with
both of those merged and before the catalog fix, passed the matrix and failed two other
steps. `Relay fills`: `relay_fill::relay_fill_range_table` took over 30 s on the runner and
failed with `publish error ... remote error: 1`; it passes here in 22 s on the same code, so
the cause is not found. `Media output and lifecycle`: the chromium `replacement` case, which
stays with the epic's replacement child.

Decided 2026-11-12 (maintainer), since green scheduled Interop is this epic's goal and stale pins caused four of the last six red runs:

- ✅ the release-remeasure pipeline is a child quest of quest/m0/interop-green/ (rejected: quest/m3/release/; drop the follow-up).
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
a test flakes epic. The fallback audio stall
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
  [resumed audio snaps to the clock](https://github.com/moq-dev/moq/pull/8635),
  decided 2026-11-12 (maintainer) to snap to the live picture rather than
  stretch the lag back.

The 11-11 run, before #8507, passed all three steps.

Scheduled runs
[43457651679](https://github.com/moq-dev/moq/actions/runs/43457651679)
(2026-11-13, at c748c0d47) and
[43606836954](https://github.com/moq-dev/moq/actions/runs/43606836954)
(2026-11-14, at bd617cc1e) show the media step moving on, and the matrix going green:

- 11-13 Media: the pause case's audio after resume, fixed by
  [moq-dev/moq#8635](https://github.com/moq-dev/moq/pull/8635), merged
  2026-11-14 after that run.
- 11-13 Interop (full matrix) and Interop (moq-transport): `js -> cpp`,
  `js -> python`, and `js -> rust` each failed once with `audio gap`, a
  different cell per matrix, with no repeat on 11-14, where both steps passed.
  This is the intermittent audio gap
  [moq-dev/moq#8633](https://github.com/moq-dev/moq/pull/8633) and
  [moq-dev/moq#8611](https://github.com/moq-dev/moq/pull/8611) track, not a
  new cause.
- 11-14 Media: only `live incumbent fallback plays` on `moq-transport-22`
  failed. The viewer's session closed with `invalid END_OF_TRACK: track is
  closed` and reconnected, so the fallback took more than its 1039 ms bound.
  The JS subscriber refused the END_OF_TRACK of a withdrawn route after its own
  unsubscribe had released the track, and a protocol violation closes the whole
  session.
  [moq-dev/moq#8769](https://github.com/moq-dev/moq/pull/8769) tolerated the marker on a released track,
  and [moq-dev/moq#8754](https://github.com/moq-dev/moq/pull/8754) moved that into the track's wire end, so a
  late PUBLISH_DONE or FETCH_OK keeps the session too.
  That is not the whole case: since [moq-dev/moq#8443](https://github.com/moq-dev/moq/pull/8443)
  dropped the Chromium quarantine, `just test media --media-cases replacement`
  at `ce616b52c` failed 4 of 14 local runs (host at load 50), as `live incumbent
  fallback plays` on `moq-lite-06` and `moq-transport-22` or the fallback audio
  tone, with no END_OF_TRACK violation in the log kept. Both failing logs show
  `skipping missing groups` on the audio track before the stall, so the audio
  stalls past the 1039 ms bound after the route falls back. Reproduced
  at 1 of 8 and 2 of 12 loaded local runs (the audio ring still stalled 600 ms after the first
  audio group of the returned incumbent). Decided 2026-11-15 (AFK, recommendation, for review):
  `replacement` is quarantined by default on every engine again, and a child quest root-caused
  and restored it. Its first PR found the stall: the decode gate released the backlog behind the
  clock one window per worklet report. Its second restored the case on Chromium, and
  [moq-dev/moq#9023](https://github.com/moq-dev/moq/pull/9023) on Firefox: the player decodes the
  audio an incumbent replays behind the clock only to warm the codec. Off Chromium the moq-transport-22 row stays quarantined, since that
  wire states no delay to cut the replay at. [moq-dev/moq#9293](https://github.com/moq-dev/moq/pull/9293)
  lifted that: the player joins live, and a relay serving a moq-transport live SUBSCRIBE asks its
  route for a live join with no min delay.

Public API: none. Wire: none.

