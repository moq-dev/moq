# [M] A relay rides out a receive blackout its subscriber's delay covers

## Goal

A subscriber whose delay covers a receive-side blackout plus normal recovery
loses no group to an `Old` reset from `moq-relay`, as on `3c0daa21`. On the
reporter's path, a `moq export ts --delay 5822ms` subscriber 117 ms from the
relay evicts no group across 3 s and 4 s blackouts, where current `main`
evicts in 16 of 18 bursts.

## Plan

Reported in [#9320](https://github.com/moq-dev/moq/issues/9320). Netem on one
host can't tell the two relays apart, so the reporter's path is the
reproduction. Their bisect of `3c0daa21..9df4313a` (1,280 relay-dependency
commits) names the commit that caused it; list it under `## Caused by`.

What the code says (2026-11-21): a group's expiry
(`GroupExpiry::is_expired` in `rs/moq-net/src/model/track.rs`) counts wall
time since its successor reached the relay, not since it was sent, and
stale bytes are released even while send credit is blocked. A backlog held
through a blackout is therefore aged by the blackout plus the time to resume
sending. With a 5.8 s budget and a 4 s blackout, about 1.8 s remains, so the
suspect is slower recovery, not earlier expiry. The reporter's qlog points the
same way (3.5–4.8 s stalls against 3.1 s). Unmeasured suspects:

- The PTO backoff in `rs/moq-quic/src/connection/mod.rs`
  (`pto_time_and_space`) after a multi-second outage, though `probe_on_heard`
  probes as soon as the peer is heard again.
- Per-stream windows (#9329, 4 MiB per stream): a 1 s group at 25 Mb/s is
  about 3 MB, so one stream's credit holds about one group.
- Live edges bounded in bytes (#9308).

Fix the cause the bisect names at its root. Do not widen the budget or
stretch expiry to hide it. Ask the reporter to confirm the branch on their
path, since netem can't.

Closed #9018 showed the same symptom at shorter stalls on `3c0daa21` itself.
Check whether its fix is in the bisect range.

## Waiting

- t0ms ([#9320](https://github.com/moq-dev/moq/issues/9320)): the overnight
  bisect names the first bad commit. Check the issue's comments.

## Closes

- [#9320](https://github.com/moq-dev/moq/issues/9320) - relay resets a 5.8 s subscriber's groups `Old` after 3–4 s blackouts
