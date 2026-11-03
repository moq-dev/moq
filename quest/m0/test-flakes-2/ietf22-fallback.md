# [M] The IETF22 viewer follows a fallback without dropping or stalling

## Goal

When the newest of two live publishers withdraws, a browser `@moq/watch`
viewer on moq-transport-22 plays the still-live older run within the media
lane's derived 1039 ms bound. It keeps its session, and its audio resumes
with its video. Both failures are fixed at their cause once one recurs with
enough evidence to reproduce it deterministically.

## Waiting

- moq-dev/moq: either failure recurs with enough evidence to isolate a
  deterministic cause. Advance it by running `just test media --media-cases
  replacement --media-engines chromium` with
  `RUST_LOG=moq_net=debug,moq_relay=debug`, and read any failure here with its
  relay and browser logs.

## Plan

Neither failure has a known cause or a reproduction yet, so no production or
test change is justified until one recurs.

Two failures were seen while landing
[moq-dev/moq#6548](https://github.com/moq-dev/moq/pull/6548), on a host at
load average 30 to 70:

- **Session drop:** 1 in 9 runs before that PR's IETF22 fixes, 0 in 37 since.
  After the fallback NAMESPACE, the viewer restarted its subscriptions. Then the
  relay stopped its own SUBSCRIBE_NAMESPACE stream to the viewer, and the
  session closed. The viewer reconnected and played the older run, but only
  after the bound. Relay debug logging was off for that run, so it cannot say
  whether the relay closed over a violation or the session closed cleanly. The
  viewer's reconnect warning now names the close reason; start there.
- **Audio stall:** 1 in 37 runs. The viewer painted the older run, and the
  relay served the older run's audio groups for the whole window. The page's
  audio stayed stalled past the bound, at the newest run's last timestamp, and
  the session stayed up.

Baseline: four focused replacement runs with Chromium 156.0.8078.4 and the
relay debug filter above passed all three protocol versions, two on 2026-10-27
at
[767685e1ba](https://github.com/moq-dev/moq/commit/767685e1ba0dabfd921fe8439d7f55ccf65eeedb)
and two on 2026-10-28 at
[487759f62a](https://github.com/moq-dev/moq/commit/487759f62a9ef39d916fe5b92df06dcff83478ce).
IETF22 replaced in 176, 186, 191, and 94 ms and fell back in 206, 209, 326,
and 228 ms, with audio resumed in sync and the incumbent identity preserved.
In the 2026-10-28 runs, the relay debug log showed one IETF22 viewer
connection throughout, resubscribing to the incumbent epoch after the fallback
NAMESPACE and closing only at teardown after grading. Neither failure
recurred. None of these runs recreated the original host load of 30 to 70, so
they are a baseline, not evidence that either failure is fixed.

Two more focused runs on 2026-11-02 (2026-11-03 UTC), at
[622d7bc0c5](https://github.com/moq-dev/moq/commit/622d7bc0c5), passed all three
versions with Chromium 156.0.8078.4. They used the same debug filter, with
`HARNESS_KEEP=1 HARNESS_RUNS="$PWD/.tmp/runs"`, in the Nix dev shell through
`quest gate`. IETF22 replaced in 168 and 169 ms and fell back in 929 and
408 ms. The browser graded both fallbacks at 30.0 fps, synchronized audio, and no
reconnect warning. Relay connection 9 sent the fallback NAMESPACE at
04:15:15.293658Z, accepted fresh catalog/audio/video subscriptions at
04:15:15.337120Z, 04:15:15.364698Z, and 04:15:15.371115Z, and closed with
`cancelled` at 04:15:20.224340Z, after playback grading. There was only one
IETF22 viewer connection in each run; the browser's namespace-stream close
message appeared only at teardown.

The one-minute host load was 30.97 near the end of the first build, before
media playback; the second invocation began at 20.16 and ended at 14.83.
These spot samples do not establish sustained load of 30 to 70 during playback.

Both runs retain their relay and browser logs in the implementation checkout's
`.tmp/runs/interop-h9Ciuig2/` and `.tmp/runs/interop-6BlHQ6Mj/`. The browser log
in each is `media-chromium--media-cases--replacement.log`; the relay log is
`relay.log`. Neither recorded failure recurred, so the cause remains unknown
and no production or test change is justified.

Public API: none expected. Wire: none expected.
