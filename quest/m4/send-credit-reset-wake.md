# [XS] Confirm whether SendCredit's model needs its reset wake

## Goal

TLC at 3 subscriptions shows whether `MakeRoom`'s wake of the reset stream's
subscription in `SendCredit.tla` ever changes reachable behavior. If it never
does, the spec may drop it, with a comment that the code's wake is still
needed; if it does, the spec keeps it. The Rust wake stays either way.

## Plan

Decided 2026-11-27 (maintainer, /quest-plan on kio's follow-ups). Milestone m4.

- ✅ Its own quest (rejected: fold it into
  [send credit machine](/quest/m4/send-credit-machine.md), which waits on kio;
  drop it). It needs only TLC, so it need not wait.
- Evidence: in `rs/moq-net/verify/credit/SendCredit.tla`, `MakeRoom` resets a
  stream and wakes its subscription (`Wake(..., {c[1]})`).
  [kixel-ai/kio#26](https://github.com/kixel-ai/kio/pull/26) found that
  removing that wake leaves reachable states and transitions identical on all
  four configs, all of which use 2 subscriptions.
- Found in review (2026-11-27), against the planning premise that the code
  has no such wake: `Lines::make_room` (`rs/moq-net/src/transport/open.rs`)
  calls `Listed::preempt` (`transport/boxed.rs`), which wakes the waiters the
  stream's writer registered in `poll_writing` and `poll_closed`. That writer
  is a `GroupServe` child in the subscription's `kio::Tasks`, so the wake
  reaches the subscription, as the model's does. The writer needs it to fail
  with `Preempted`, so the code keeps it.
- Compare reachable states and transitions with and without the wake on a
  3-subscription config. Keep that config only if TLC's time stays in the
  `credit` leg's budget.

Public API: none. Wire: none.

## Waiting

- Maintainer: the planning round kept this quest to make the spec match code thought to lack the reset wake, but the code has it (`Listed::preempt` wakes the reset stream's writer, a child task of its subscription), so neither outcome changes the code. Keep the quest?
  - Drop it (Recommended) - the spec already models the code's wake, so removing it from the spec only makes the model less faithful
  - Keep it - confirms at 3 subscriptions that the model's wake is redundant, for a possibly smaller spec
