# [S] JS sessions take a link cost and priority

## Goal

`@moq/net` prices and ranks each link like Rust does. `Connection.connect()`
and `accept()` take a per-session link `cost`, which mirrors Rust's
`Client::with_cost` (`rs/moq-net/src/client.rs`). Rust's server gains the
matching `with_cost`, so both sides of a session can price it in both
languages. The JS origin producer gains a `priority`, which mirrors
`origin::Producer::with_priority` (`rs/moq-net/src/model/origin.rs`) and
ranks the link that a session lands its routes through. `#candidate` in `js/net/src/lite/routed.ts` uses both
in place of the hardcoded `claim.cost + 1n` and `Math.min(0, claim.priority)`.
An unpriced link still costs `1` and ranks at the neutral `0`, so existing
callers keep shortest-path routing.

## Plan

Decided 2026-11-25 (maintainer, /quest-plan via kixel-ai/kixel.ai): ✅ m1,
since it is additive (rejected: m3 beside the cluster-routing epic). Kixel
routes its mesh by link quality and needs a JS endpoint to price and rank
its own links, as a Rust one can. A link's cost is declared in SETUP as Rust
declares it. A price we set wins over the one the peer declares. Zero is
refused on moq-lite-07, as in Rust. The same order, our price, then the
peer's SETUP price, then `1`, applies on lite-06 too. That version's announce
path reads the advertised cost apart from `#candidate`, so the change and
its test cover both versions.

Decided 2026-11-25 (maintainer, on
[moq-dev/moq#9666](https://github.com/moq-dev/moq/pull/9666)): ✅ `cost` on
connect and accept in both languages, adding Rust's server-side `with_cost`,
and the JS `priority` on its origin producer, as Rust's is (rejected: both on
connect and accept in both languages, which would move Rust's
`with_priority` off the origin and break its API). This keeps moq's rule
that API names mirror across Rust and JS.

A price that a server sets on accept wins over the one its client declares,
just as a dialer's does. A JS derived origin handle inherits its parent's
priority, as in Rust.

Test it on lite-06 and lite-07 with two links to one Instance: the lower-cost one wins, a
higher-priority one wins despite its cost, and on lite-07 the route ranks by
the lowest priority on its path.

## Related

- [Cluster routing](/quest/m3/cluster-routing/README.md) - the routing model these costs and priorities feed
- [Claim arrival rank](/quest/m3/claim-arrival-rank.md) - the re-ranking a changed cost or priority runs
- [Session keep-alive](https://github.com/moq-dev/moq/pull/9698) - Kixel's other link-quality primitive
- [Grant cost](/quest/m1/grant-cost.md) - the relay's side of pricing a session
