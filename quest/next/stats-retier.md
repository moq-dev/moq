# [M] A re-checked tier retags a live session's stats

## Goal

When an auth server's `revalidate` reply changes a session's `tier`, the
session's presence and subsequent traffic record under the new tier without
reconnecting. Today the relay logs the change and keeps the admitted tier
until the client reconnects, because the stats carriers resolve their
`(path, tier)` counters once at admission.

## Plan

Unplanned. The shape to weigh: `moq_net::stats::Session` holds its tier
behind a lock or an `ArcSwap`, moves its presence gauge from the old tier's
counters to the new on a swap, and `Scope`/`Meter` either re-resolve on the
next bump (a lookup per bump, which the payload path has avoided) or hold an
`Arc` the session swaps. Traffic already counted stays where it was. The
relay's `auth::Lease::ended` (`rs/moq-relay/src/auth.rs`) then applies the tier instead of logging it, and
`tests/auth_lifetime.rs` asserts the swap through the stats registry. Run
`/plan-quests` on this file.

## Related

- [In-band auth](/quest/next/auth/README.md) - a re-checked tier through an in-band token follows the same path
- [QoS](/quest/next/qos/README.md) - the stats these tiers bucket
