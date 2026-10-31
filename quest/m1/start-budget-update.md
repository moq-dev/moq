# [S] A tightened SUBSCRIBE keeps history below the start fillable

## Goal

A relay copy never infers which history is missing from the budget its
publisher resolved SUBSCRIBE_START under. If a SUBSCRIBE_UPDATE crosses the
start in flight, a later reader whose budget reaches below the start still
fills that history from upstream. No wire changes.

## Plan

Decided 2026-10-31 (maintainer, rethink of
[moq-dev/moq#7235](https://github.com/moq-dev/moq/pull/7235)): drop the
history inference instead of adding a lite-07 START field. The earlier
attempt echoed the selecting Subscriber Max Age in START so the relay could
record which budget skipped the groups below it; that field is removed from
Rust, JS, and the draft.

The copy now records only that its start was resolved by the publisher.
Every group below it stays fillable: each reader judges a hole by its own
budget against the copy's media time, and a fill that finds nothing ends
empty. Lite-06 copies keep relying on the publisher's re-offer, since their
fills cannot carry a budget.

Keep the direct and relayed integration test: tighten before the start is
selectable, then an intermediate-budget reader fills the history between
the two starts while the publisher is paused.

## Related

- [Drop set](/quest/m1/drop-set.md) - records the budget each drop was judged under
- [Delete the old start bookkeeping](/quest/m1/subscribe-ranges/start-bookkeeping.md) - moves where a subscription starts into the model
