# [S] PUBLISH token renewal is consistent across pause and forward, and the multi-token cost is profiled

## Goal

On a moq-transport session, a REQUEST_UPDATE carrying an AUTHORIZATION
TOKEN on a PUBLISH gets the same answer whether the PUBLISH is paused or
forwarding: a renewal is accepted in both states, or refused in both. A
serve-level test covers a token on a paused PUBLISH offer and on a
forwarding one. The remaining multi-token gap in
`cargo bench -p moq-net --bench auth -- auth/request_tokens` is profiled,
and its cause either fixed or recorded in this quest's PR with numbers.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7211](https://github.com/moq-dev/moq/pull/7211),
which finishes [request tokens](/quest/m1/ietf-token-auth.md).

- Renewal: after [moq-dev/moq#7211](https://github.com/moq-dev/moq/pull/7211) a paused PUBLISH accepts a token, but a forwarding
  one refuses it ("AUTHORIZATION TOKEN on a locally initiated PUBLISH",
  in `apply_subscription_update`, `rs/moq-net/src/ietf/publisher.rs:2130`
  on main), so a forwarding PUBLISH can't be renewed in place. It fails
  closed, so this is a gap, not a hole. Recommended: accept the renewal
  while forwarding, held to the same checks [moq-dev/moq#7211](https://github.com/moq-dev/moq/pull/7211) applies to a paused
  PUBLISH's token. Refusing it while
  paused too is the fallback if acceptance needs a new grant path.
- Profile: single-token rounds match main; multi-token rounds are 1.5 to
  2.2x slower and grow with tokens times requests. The suspected cost is
  each announce polling its guard on the shared verdict. Profile with
  `samply` or `perf` on the bench before changing anything.

Public API: none. Wire: none; behavior only.

## Required

- [A moq-transport request token is one auth per distinct token](/quest/m1/ietf-token-auth.md) - its PR, [moq-dev/moq#7211](https://github.com/moq-dev/moq/pull/7211), adds the shared grants and paused-PUBLISH token this extends
