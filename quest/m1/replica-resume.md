# [S] A lagging same-epoch replica resumes on every epoch wire

## Goal

One publisher announced through two relays under its epoch, one of them
lagging: when the route a subscriber reads dies mid-group, the subscriber
resumes on the other route and sees every frame exactly once, in order. This
holds on lite-07 and on moq-transport with the MoQ Epoch extension (draft
17+), since epochs ride moq-transport as of
[#5932](https://github.com/moq-dev/moq/pull/5932).

## Plan

Decided with the maintainer on 2026-10-20. Today
`lite_resumes_after_a_lagging_route_dies`
(`rs/moq-net/tests/integration/route_change.rs`) covers the lagging case on
lite-07 only, and the route-flap tests cover moq-transport 22 without a lag.

- Run the lagging scenario on moq-transport 17 and 22 as well. Lite carries
  the resume point upstream; find what moq-transport carries instead, and fix
  a duplicate or gap it shows at its cause rather than weakening the
  assertion.
- A shared epoch here means true replicas of one publisher's bytes, never two
  independent publishers.
- Mirror the case in `js/net` if its resume tests can drive a lagging route.

Mocked time, wired into CI with the other integration tests.

Public API: none. Wire: none.

## Related

- [Claim answer epochs](https://github.com/moq-dev/moq/pull/5678) - changed how a claim answers, so this contract is rechecked after it
- [Resume through an outage](/quest/m1/resume-outage.md) - the same-epoch resume after an unclosed route dies
