# [S] Rust cancels an unclaimed fill head when a subscription ends

## Goal

When SUBSCRIPTION_ENDED arrives while a fill head is still unclaimed, Rust's
moq-transport subscriber cancels that fill instead of finishing it, as the JS
subscriber does since [moq-dev/moq#7089](https://github.com/moq-dev/moq/pull/7089),
so a reader never sees a clean end for a group it was never sent.

## Plan

Planned 2026-10-30 (maintainer, `/quest-plan`). The finish is `Fill::end` in
`rs/moq-net/src/ietf/subscriber.rs`. Add a test that ends a subscription with
an unclaimed fill head and expects the cancel, in both languages' terms.

[IETF fill refill](/quest/m2/ietf-fill-refill.md) would later remove this
fill handoff entirely; this fix is for the release cut, not a reason to wait.

## Related

- [IETF fill refill](/quest/m2/ietf-fill-refill.md) - replaces the fill claim and handoff this touches
