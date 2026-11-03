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
  relay and browser logs. Record host load during playback; clean runs do not
  clear this wait, even when the host is busy.

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

The latest two focused runs, on 2026-11-02 (2026-11-03 UTC) at
[622d7bc0c5](https://github.com/moq-dev/moq/commit/622d7bc0c5), also passed all
three versions with Chromium 156.0.8078.4. They used the debug filter above,
`HARNESS_KEEP=1 HARNESS_RUNS="$PWD/.tmp/runs"`, and the Nix dev shell through
`quest gate`. IETF22 replaced in 168 and 169 ms and fell back in 929 and
408 ms. Both fallbacks graded at 30.0 fps with synchronized audio. Each run
had one IETF22 viewer session, no reconnect warning, and a close only after
grading, at teardown.

The first run's **929 ms fallback had only 110 ms of headroom**. Its relay
connection 9 logged these events on 2026-11-03 (shared prefix and broadcast
name omitted):

```text
04:15:15.293658Z namespace broadcast=replacement-moq-transport-22-2960283.hang
04:15:15.337120Z subscribe started id=16 track=catalog.json
04:15:15.364698Z subscribe started id=18 track=audio
04:15:15.371115Z subscribe started id=20 track=video
04:15:15.397449Z serving group subscribe=18 track=audio sequence=58
04:15:20.224340Z connection closed id=9 err=cancelled
```

All three resubscriptions began within 78 ms of the fallback NAMESPACE, and
audio serving began within 104 ms. The browser logged skipping missing/slow
audio groups through sequence 499 before its successful fallback grade. These
logs do not isolate the remaining delay among delivery, decoding, and playout;
the near-bound sample warrants investigation, not a claim that either original
failure is fixed. The second run's connection 9 sent fallback NAMESPACE at
04:16:47.151856Z and began audio/video subscriptions at 04:16:47.162606Z and
04:16:47.163028Z, closing only at 04:16:51.571821Z after grading.

The one-minute load was 30.97 before the first run's media playback, and 20.16
before the second invocation and 14.83 after it. These spot samples do not
establish sustained load of 30 to 70 during playback. The six clean focused
runs recorded here remain baseline evidence; neither original failure recurred.

Public API: none expected. Wire: none expected.
