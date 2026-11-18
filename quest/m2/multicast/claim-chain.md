# [M] Record each grant's exporter and adopter in a claim's chain

## Goal

A lite-07 claim's chain names the announcing relay, then each grant's
exporter and adopter, so a node refuses a claim naming it in the role it
would take, as the route proof assumes.

## Plan

Found 2026-11-16 by [the proof](https://github.com/moq-dev/moq/pull/8904)
against the prototype; proposed by
[moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053). Today the
router and the draft record only the adopters (the Relay of each routing
node that announced the claim again across a scoped grant or from an earlier
version), so they refuse fewer echoes than the proof assumes.

Decided 2026-11-18 (maintainer, on
[moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053#issuecomment-6975349236)):
✅ the chain change lands before the lite-07 cut, split out of
[the proof binding](/quest/m3/route-proof-binding.md), whose rest follows the
cut (rejected: the whole binding before the cut; dropping the chain change).

- Change ANNOUNCE_START's chain in `drafts/draft-lcurley-moq-lite.md`
  (Claims and the `Chain Count` field) to the announcing relay first, then
  each grant's exporter and adopter, oldest first, with the 32 limit and the
  refusal rule restated for it: a node refuses a claim whose chain names it
  as the exporter or adopter it would be.
- Make `route::relay::Router` and the lite-07 claim codec
  (`rs/moq-net/src/lite/announce.rs`) record and check the new chain, and
  `js/net`'s decoder read it, with the corpus (`rs/moq-net/fixtures/lite.json`)
  and fuzz seeds updated.
- Add a test of an echo the old chain let through: a claim that crosses a
  grant and comes back to its exporter is refused there.

Public API: none. Wire: what a lite-07 claim's chain carries.

## Related

- [Cache-aware multicast](/quest/m2/multicast/README.md) - the epic, which the lite-07 cut waits on
- [Proof binding](/quest/m3/route-proof-binding.md) - the rest, which follows the cut
