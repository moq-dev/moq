# [M] The IETF22 viewer follows a fallback without dropping or stalling

## Goal

When the newest of two live publishers withdraws, a browser `@moq/watch`
viewer on moq-transport-22 or moq-lite plays the still-live
older run within the media lane's derived 1039 ms bound. It keeps its session, and its audio resumes
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

Recurrence, added 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7667](https://github.com/moq-dev/moq/pull/7667)): Nightly's
Firefox media job failed `live incumbent fallback plays` on two consecutive
nights with the audio-stall symptom (`audioStalled:true, audioTimestamp:0`
after the fallback), on `old-moq-lite-07-wip`:
[2026-11-02](https://github.com/moq-dev/moq/actions/runs/41483654426/job/125408204778)
and
[2026-11-03](https://github.com/moq-dev/moq/actions/runs/41656613559/job/125988510133).
[moq-dev/moq#7716](https://github.com/moq-dev/moq/pull/7716) found it
separate from the Firefox tone problem. Scheduled Interop runs hit the same
stall in Chromium, also on `old-moq-lite-07-wip`:
[41495461904](https://github.com/moq-dev/moq/actions/runs/41495461904),
[41668668824](https://github.com/moq-dev/moq/actions/runs/41668668824), and
[41863118968](https://github.com/moq-dev/moq/actions/runs/41863118968). Read those jobs' artifacts first. If
they isolate the cause, the Waiting item clears. If they don't, rerun the
Waiting section's command with `--media-engines firefox` and the same
`RUST_LOG` filter.

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

Public API: none expected. Wire: none expected.
