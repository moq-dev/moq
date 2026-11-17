# [L] A live subscriber's track passes stop meeting on the track lock

## Goal

The relay's publisher serves a live subscriber's track side without
contending for the track's shared state lock: in `chat` and `fanout`, futex
instructions per delivered frame fall to `release`'s level (about 2.3k and
12.6k on 8 workers) at 2, 8, and 16 workers, and relay CPU per frame is no
higher than `release`'s. Drop forwarding and fills behave as before. Nothing
here changes an API or the wire.

## Plan

Found by [Relay CPU](https://github.com/moq-dev/moq/pull/8979), which
recorded the numbers in
[release-perf.md](/.github/release-perf.md#relay-cpu-2026-11-16) and accepted
the rest of the cost for the cut.

Facts:

- Every subscriber of a track wakes on each group at once. Its
  `TrackRun::poll_track` (`rs/moq-net/src/lite/publisher.rs`) then runs three
  passes per group (the arrival, the pass after serving it, the finish),
  forwarding drops five times (`Subscriber::poll_drop_probing`) and judging
  misses six (`Cursor::probe`, `Misses::judge`). Each takes the track lock
  separately for the drift edge (`Cursor::poll_drift`), the judgement, and the
  drop cursor (`DropCursor::next`), and the read path takes it again
  (`poll_peek_group`, `take`, `forget_past_budget`).
- On 8 tokio workers, `chat`'s futex instructions per frame were 2.3k on
  `release`, 8.5k on `main`, and 4.7k once the drop poll stopped reading the
  stale revision it rarely needs. `fanout`'s were 12.6k, 33.8k, and 32.9k:
  its 64 subscribers per track contend in the passes themselves, and its
  forward-drop path is 65k instructions per frame, 30% of the relay.

Approach, in order of reach:

- A pass that changed nothing judges nothing: the drop cursor keeps the track
  version (`kio::Channel::version`, as `kio::Watch` does) its last pending
  judgement saw, with the cursor inputs it read (`next_key`, the floor, the
  window's revision, the record, cuts, the subscription's budget), and parks
  again at an unchanged version without the lock. A pending deadline (the
  misses' deferred gaps, `watch_wall`) wakes without a version change, so a
  cursor waiting on one judges again.
- One pass takes the lock once: the drift edge, the judgement, and the drop
  cursor's next run share one guard.

Measure futex and total instructions per frame in both workloads with
`just bench-profile instructions chat "2 8 16" tokio-workers origin/main`,
and relay CPU with `just bench-relay origin/release quic`, both before and
after; `just bench-fanout` checks the single-threaded count did not grow.

## Waiting

- Maintainer: whether the cut waits on this. Recommended: no, it follows the
  cut in m3. With the fix in
  [moq-dev/moq#8979](https://github.com/moq-dev/moq/pull/8979), relay CPU per
  frame against `release` reads a median +8.7% in `chat` and +18.8% in
  `fanout` (signs vary across pairs) and -15.2% in `video`, memory is below
  `release`, and the change reworks the drop cursor's judgement, which is too
  large to review beside the cut. Otherwise `quest mv` it into m2.

## Related

- [kio channel contention](/quest/m9/kio-channel-contention/README.md) - the general lock-wait work this narrows to one channel
- [Performance audit](/quest/m3/audit-perf.md) - reruns the release pairing after the cut
