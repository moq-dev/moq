# [M] The browser viewer follows a fallback without dropping or stalling

## Goal

When the newest of two live publishers withdraws, a browser `@moq/watch`
viewer on moq-transport-22 or moq-lite plays the still-live
older run within the media lane's derived 1039 ms bound. It keeps its session, and its audio resumes
with its video. Both failures are fixed at their cause once one recurs with
enough evidence to reproduce it deterministically.

## Waiting

- Maintainer: whether this PR grows into the transport fix the Plan describes,
  or the replay becomes its own quest that this one requires. Recommended: fix
  it here, since this quest exists for the failure and the repository lands
  the whole fix in one PR. An agent did not pick because the cause is pinned to
  the JS publisher and Rust relay leg, not yet to one side, and either fix
  changes `js/net` or `rs/moq-net` serving semantics for every subscriber.
- moq-dev/moq: the session drop recurs with relay debug logs and the viewer's
  close reason. It has not recurred since
  [moq-dev/moq#6548](https://github.com/moq-dev/moq/pull/6548)'s IETF22 fixes;
  clean runs do not clear this wait.

## Plan

**Cause of the audio stall: a resubscription replays the publisher's retained
history ahead of the live edge.** Seen in a passing local run on 2026-11-05
(Firefox player, `RUST_LOG=moq_net=debug,moq_relay=debug`, load average 21 to
29). When the replacement took over, the relay's upstream audio subscription to
the older JS publisher went idle (20:26:09.71) with its copy at group 95. On the
fallback 8.4 s later, the viewer subscribed with `max_delay: 130ms`, no start
group, and descending order. The relay served it group 95 from the stale copy,
then about 450 groups (96 to 543, about 9 s of 20 ms Opus groups) that the
publisher burst upstream, before settling on the live edge at one group per
20 ms. The burst took 564 ms to drain, and the player received and skipped every
group (`skipping missing/slow groups` 191 to 542) before audio resumed. The
fallback graded at 933 ms against the 1039 ms bound. moq-lite-06 replayed the
same way (104 to 535, drained in 184 ms), and so did each first join, about
1.5 s (4 to 81). The replay happens every time; only its drain time varies with
load, and CI's drain outlasts the bound.

A Rust publisher behind the same relay does not replay: a probe of
`rejoin_skips_a_cache_kept_by_another_handle`
(`rs/moq-net/tests/integration/rejoin.rs`) with a 130 ms max delay and 400 groups
20 ms apart delivered 8 groups before the edge on lite-05, 06, and 07-wip, and 1
on moq-transport-18 and 22. So the defect is on the leg between the JS lite
publisher (`js/net/src/lite/publisher.ts`) and the Rust relay. Either the
publisher serves retained groups the relay's request does not ask for, or the
relay forwards them although the publisher's answer carries the largest
position (`publisher.ts:1550`). Next: reproduce with a JS publisher behind a Rust
relay, assert a rejoin starts within the subscriber's max delay of the edge, then
fix the side that is wrong. [moq-dev/moq#7737](https://github.com/moq-dev/moq/pull/7737)
fixed the lite-03/04 floor for an unresolved copy; this is the lite-06+ path.

**Audio stall in CI, 2026-11-02 to 2026-11-05.** It failed `live incumbent
fallback plays` seven times: Nightly's Firefox player
[41483654426](https://github.com/moq-dev/moq/actions/runs/41483654426/job/125408204778),
[41656613559](https://github.com/moq-dev/moq/actions/runs/41656613559/job/125988510133),
and [42050683248](https://github.com/moq-dev/moq/actions/runs/42050683248/job/127325638704),
and scheduled Chromium Interop
[41495461904](https://github.com/moq-dev/moq/actions/runs/41495461904),
[41668668824](https://github.com/moq-dev/moq/actions/runs/41668668824),
[41863118968](https://github.com/moq-dev/moq/actions/runs/41863118968), and
[42062226837](https://github.com/moq-dev/moq/actions/runs/42062226837).
[moq-dev/moq#7716](https://github.com/moq-dev/moq/pull/7716) found it separate
from the Firefox tone problem. Every one failed on `moq-lite-07-wip`, but the
case runs that version first and stops at its first failure, so the protocol is
confounded with order, and nothing yet ties the stall to any version.

The four failure samples read (both 2026-11-05 runs, 41668668824, and
41863118968) agree on everything but the playhead. The session stayed up, the
broadcast was live on the older instance, and video kept painting. The
AudioContext was `running`, the player was not paused, and the sync delay was
50 ms. The viewer closed its audio subscription and opened one on the
older run within the same announce, and its container consumer kept reading and
skipping that run's groups (`skipping missing/slow groups` through sequence 549
in Firefox and 602 in Chromium) until the bound. Audio stayed `audioStalled`
with zero RMS. The stalled `audioTimestamp` differed:

- **0** in both 2026-11-05 runs: the instance change reset the ring
  (`#runDecoder` in `js/watch/src/audio/decoder.ts`), and no PCM reached the
  worklet afterwards, since its first write anchors the playhead.
- **11584.7 ms**, on the older run's timeline, in 41668668824: PCM landed and
  anchored, then the ring stalled.
- **4179.0 ms**, on the newest run's timeline, in 41863118968: the worklet
  either never saw the reset or stopped reporting. This matches the original
  report of a stall "at the newest run's last timestamp".

These fit the replay: when the bound expired, each viewer was still receiving
and skipping the older run's backlog, so its ring had not refilled, and the
differing playheads show only how far each had got. The CI artifacts could not
show this on their own: scheduled runs keep the relay at info (45 or 46 lines of
`relay.log`), and the harness traces only pages that call `startTrace`, which
the replacement player does not.

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
