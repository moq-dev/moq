# [L] moq-transport hole markers

## Goal

A negotiated moq-transport extension (a new `drafts/draft-lcurley-moq-*`
document) lets a publisher name each group it won't deliver on a SUBSCRIBE,
like moq-lite's SUBSCRIBE_DROP: never produced, or dropped with its reason.
A relay's copy ingested over moq-transport then knows its holes ("null")
instead of leaving them unknown ("undefined"). Our moq-transport publishers
then answer a multi-group FETCH, and a multi-group draft-22 fill, whenever
the cache knows every group in the range: a local, moq-lite, or
extension-carrying copy. Groups go out in order. End of Non-Existent Range
(`0x8C`) marks only a proven hole: a group never produced, or a
budget-independent drop (a cut, an origin's eviction or expiry). A stale
drop judged under a narrower budget or range, as in
[Drop set](/quest/m1/drop-set.md), proves nothing for a wider FETCH. That
group is unknown, so the publisher fills it or the range is refused. Any
other range stays refused, as
[moq-dev/moq#7204](https://github.com/moq-dev/moq/pull/7204)
leaves it.

## Plan

Decided with the maintainer on 2026-10-28 (`/quest-plan`, "what's not worth
supporting in IETF").

Decision prompts:

- Define a hole-marker extension: no, since drafts 18 and 22 already have
  `0x8C` and `0x10C`; ✅ yes. The marker belongs on the SUBSCRIBE, so that
  the cache can be used for FETCH. FETCH's own markers only describe the
  answer, not the copy that answers it.
- Scope: ✅ extension plus range FETCH from caches that know every hole, in
  m2; extension only; both in m1.

Only peers that implement the extension benefit, which today means our own
Rust and JS moq-transport stacks. Our own peers talk moq-lite to each other,
so the payoff is third-party adoption. Write the draft so it can be proposed
to the working group.

Open, for this quest's interview:

- The carrier: a message on the SUBSCRIBE's request stream (drafts 18 and 22
  give each request its own stream), or a status on a group stream.
- Whether the drop names its budget, as lite-07's SUBSCRIBE_DROP `Max Age`
  does in [Drop set](/quest/m1/drop-set.md), so a stale drop doesn't settle
  a wider reader.
- How a publisher answering a range walks the drop set's runs in group
  order, and where FETCH_OK's End Location falls.

Test across Rust and JS on drafts 18 and 22, with and without the extension.
A range over a copy whose holes are all proven is answered with `0x8C` runs.
A range with an unknown hole is still refused. Regression: an upstream
reports G stale for a narrow subscriber while still holding G; a wider
downstream FETCH covering G gets G, or a refusal, never `0x8C`. Interop with a peer that lacks
the extension is unchanged.

Public API: none. Wire: a new negotiated SETUP option and its drop message.
Our publishers answer multi-group FETCH and fills over known caches.

## Required

- [Drop set](/quest/m1/drop-set.md) - the per-track runs that say which groups are known holes
