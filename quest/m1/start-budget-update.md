# [S] A tightened SUBSCRIBE keeps the start's budget honest

## Goal

A relay copy remembers the `max_delay` that was in effect when its
publisher resolved SUBSCRIBE_START, not only the one sent at SUBSCRIBE. If a
SUBSCRIBE_UPDATE tightens the delay before the start is picked, a later
reader whose delay is looser than the tightened one, but no looser than the
original, still fills the history between the two starts. Released wires are
unchanged.

## Plan

Decided 2026-10-29 (maintainer, `/quest-plan` of moq.pro's announce ramp
loss follow-ups): its own quest in m1, rather than waiting on
[Drop set](/quest/m1/drop-set.md) or
[Delete the old start bookkeeping](/quest/m1/subscribe-ranges/start-bookkeeping.md),
since it fixes released lite-06 behavior.

Found by review of [#7168](https://github.com/moq-dev/moq/pull/7168): the
lite subscriber sets `requested_max_delay` once, at SUBSCRIBE
(`rs/moq-net/src/lite/subscriber.rs`). `Misses` then gives up every group
below the start for any reader whose budget is no looser than that value.
After a tightening update, the start was resolved under the tighter budget,
so the recorded looser one makes some readers skip history they could have
filled. It loses history only in that window; it does not bring back #7168's
over-fetch.

Record the budget the start was actually resolved under. Track it per
update, or take it from the update in effect when SUBSCRIBE_START arrives.
Add an integration test in `rs/moq-net/tests/integration`, direct and
through a relay: subscribe, tighten before the start, then a second reader
widens partly. It fails on `main` and passes with the fix.

## Related

- [Drop set](/quest/m1/drop-set.md) - records the budget each drop was judged under, the same idea for drops
- [Delete the old start bookkeeping](/quest/m1/subscribe-ranges/start-bookkeeping.md) - moves where a subscription starts into the model, which should carry this field
