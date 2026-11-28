# [M] A group every subscriber passes over stops pausing its ingest

## Goal

Relay ingest pauses a group nobody has picked up yet only while some
subscriber's ranges still admit it. A group every cursor passes over, such as
one a subscriber narrowed away from while another subscriber with other ranges
stays attached, reads on and sheds instead of parking until it expires.

## Plan

Decided 2026-11-27 (maintainer, review of
[moq-dev/moq#9770](https://github.com/moq-dev/moq/pull/9770)): accept the gap
there and fix it here. Decided 2026-11-28 (maintainer, `/quest-plan` of the
pre-0.22 design-gate audit): ✅ pulled back from `m3` to `m0` gating `0.22.0`
(reversing [moq-dev/moq#9842](https://github.com/moq-dev/moq/pull/9842)), since
`cut.md` gates `0.22.0` on paused-stream receive-credit safety and `verify-credit`.
While touching `group.rs` and `pace.rs`, also fold `Pace::held: AtomicBool`
(`rs/moq-net/src/model/pace.rs:63`) into `GroupState` so first-reader pickup and
consumer registration live under one lock per `REVIEW.md` instead of split `SeqCst`
atomics.

Caused by [moq-dev/moq#9770](https://github.com/moq-dev/moq/pull/9770).

#9770 made a group no reader picked up yet owed to its track's subscribers:
`group::Handle::owed` and `Handle::poll_unowed` (`rs/moq-net/src/model/group.rs`)
count it as owed whenever the track has any consumer. `owe_arrivals`
(`rs/moq-net/src/model/track.rs`) passes a group outside a cursor's ranges or
past its cap without consuming it, so when every cursor passes it over, nothing
ever picks it up. `poll_paced` then stays Pending until the group expires or
aborts, and the paused stream pins its paused-budget credit (up to half the
connection window) meanwhile, the stalled-credit case
`rs/moq-net/AGENTS.md` warns about.

- Count a group as owed only while some cursor still owes its key: have a
  cursor that passes over a group mark it the way a write-off does, so the
  group knows once every cursor of the track passed it, or give the track a
  per-key query of whether any subscriber's ranges admit the key.
- Model the pass-over in `rs/moq-net/verify/credit/ReceiveCredit.tla`:
  `Arrive` is weakly fair, which assumes every owed group is picked up. Add a
  pass-over action and check that `EveryPauseFits` and `ChargedAllowance` still
  hold, then rerun every config the verify-credit skill lists.
- Test: two subscribers on one relay track, one narrows away from a group
  mid-ingest while the other's ranges exclude it, and ingest finishes the group
  without waiting for expiry.

Public API: none. Wire: none.
