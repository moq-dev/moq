# [M] The IETF22 viewer follows a fallback without dropping or stalling

## Goal

When the newest of two live publishers withdraws, a browser `@moq/watch`
viewer on moq-transport-22 plays the still-live older run within the media
lane's derived 1039 ms bound. It keeps its session, and its audio resumes
with its video. Both failures are fixed at their cause once one recurs with
enough evidence to reproduce it deterministically.

## Plan

Waiting on a recurrence: run `just test media --media-cases replacement
--media-engines chromium` with `RUST_LOG=moq_net=debug,moq_relay=debug`, and
read any failure here. Neither failure has a known cause or a reproduction yet.

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

Public API: none expected. Wire: none expected.
