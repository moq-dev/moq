# [S] JS sessions take a link cost and priority

## Goal

`@moq/net` prices and ranks each link like Rust does. `Connection.connect()`
and `accept()` take a per-session link `cost`, which mirrors Rust's
`Client::with_cost` (`rs/moq-net/src/client.rs`). The session's link also
gets a `priority`. `#candidate` in `js/net/src/lite/routed.ts` uses both
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

Rust sets a link's priority on the origin handle given to the session
(`origin::Producer::with_priority` in `rs/moq-net/src/model/origin.rs`), not
on the client, and its server has no `with_cost`. `@moq/net`'s origin has no
priority. So the decided `connect()`/`accept()` placement breaks moq's rule
that API names mirror across Rust and JS. See Waiting.

Test it on lite-06 and lite-07 with two links to one Instance: the lower-cost one wins, a
higher-priority one wins despite its cost, and on lite-07 the route ranks by
the lowest priority on its path.

## Waiting

- Maintainer: where the JS priority (and an accept-side cost) live, given
  that Rust puts priority on the origin handle and has no server-side cost.
  Options: (a) `cost` on both `connect()` and `accept()`, plus a matching
  Rust `with_cost` on the accepted request; `priority` on the JS origin
  producer, as Rust's `with_priority` is (recommended: the names mirror
  across both languages, and one origin handle per link already carries the
  rank in Rust); (b) both on `connect()`/`accept()` in JS as decided, with
  Rust left as is, so the two languages diverge; (c) both on
  `connect()`/`accept()` in JS, and Rust's priority moves from the origin to
  `Client`/`Server`, which breaks Rust's API and moves the quest to m2.

## Related

- [Cluster routing](/quest/m3/cluster-routing/README.md) - the routing model these costs and priorities feed
- [Claim arrival rank](/quest/m3/claim-arrival-rank.md) - the re-ranking a changed cost or priority runs
- [Session keep-alive](/quest/m1/keep-alive.md) - Kixel's other link-quality primitive
- [Grant cost](/quest/m1/grant-cost.md) - the relay's side of pricing a session
