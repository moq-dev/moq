# [M] lite-07 sessions stop hanging on a dropped token's route

## Goal

The lite-07 benches that hang on `main` complete: moq-net `auth/waiting/moq-lite-07-wip/1/1`,
moq-relay `session` lite-07 delivery credit, moq-relay `relay_auth_refresh/1/1`,
and the browser bench smoke ("control kept 8 broadcasts after concluding").
The root cause is fixed, and a deterministic test (a kio simulation or a
model-checked case) fails without the fix. lite-03 to lite-06 are out of scope.

## Plan

Found by the triage of Nightly run
[44127032469](https://github.com/moq-dev/moq/actions/runs/44127032469)
(2026-11-17). Bisected over 7 steps and reproduced locally: caused by
[moq-dev/moq#8835](https://github.com/moq-dev/moq/pull/8835). In
`auth/waiting` under real tokio, the client adds a token, the server grants
everything, the client announces a hidden route, then drops the token. On the
13th repetition the server never hears the route. It is deterministic in the
bench, but a kio simulation of the same steps passes 60 rounds, so a lost
wakeup or a timing-dependent ordering in `rs/moq-net/src/lite/subscriber/route_stream.rs`
or `routed.rs` is likely; `Running::poll`'s permit, ask, and edit path around
the 13th grant is the first place to trace. The relay and browser hangs share
the range and the lite-07 routing and auth paths but were not bisected.

Decided 2026-11-17 (maintainer, /quest-plan):

- Goal: ✅ fix every bench above with one deterministic regression test
  (rejected: only the bisected bench, with the others getting their own quests).
- Order: ✅ re-run the benches on
  [moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987)'s head first,
  since it rewrites `route_stream.rs` and `routed.rs`. If they still hang, fix on
  top of #8987 after it lands; if not, add only the regression test (rejected:
  fix on `main` now, which #8987 would likely throw away).
