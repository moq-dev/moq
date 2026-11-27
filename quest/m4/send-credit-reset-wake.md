# [XS] Confirm and drop SendCredit's reset wake

## Goal

TLC at 3 subscriptions confirms that `MakeRoom`'s wake of the reset stream's
subscription never changes reachable behavior, and `SendCredit.tla` drops it
to match the code. If 3 subscriptions shows it matters, the code gains the
wake instead, as a bug fix with a test that fails without it.

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
- Unverified: `Lines::make_room` in `rs/moq-net/src/transport/open.rs` seems to
  have no such wake. It calls `stream.preempt()`, which wakes that stream's
  own waiters. Confirm what `preempt` wakes before deciding which side changes.
- Compare reachable states and transitions with and without the wake on a
  3-subscription config. Keep that config only if TLC's time stays in the
  `credit` leg's budget.

Public API: none. Wire: none.
