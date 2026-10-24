# [L] Lite-07 ranges

## Goal

moq-lite-07's SUBSCRIBE carries group ranges and an order, SUBSCRIBE_UPDATE
replaces them, and FETCH is gone from lite-07. The Rust publisher serves
ranges from the model and drops what it won't deliver, and a relay fills a
lite upstream by subscribing to the missing ranges.

## Plan

Each range end encodes `Absolute(group, frame)`, `Relative(n)`, `Time(pts)`,
or open, and a SUBSCRIBE with no ranges is refused. A `Time` end has no
older-wire mapping: the answering hop resolves it before going upstream on
lite-06 or earlier, or refuses it. Older wires map at the codec only, with
no change to a published version: lite-06 `Group Start` 0 with `Frame Start`
0, and pre-06 absent, are `Absolute(0)..` capped by max age (the oldest group
not yet stale); any other lite-06 pair is an absolute start. When merged
ranges can't be expressed on an older upstream wire, subscribe from group 0
and filter locally; lite-03 to 05 must send an explicit group 0, since the
codec folds 0 to absent (`encode_start_group`), which they read as the latest
group. moq-transport's relative offsets differ by one: draft-20
`Relative(0)` is the next group and `Relative(1)` the current one, so an
open-ended `Relative(n)..` maps to draft-20 `Relative(n + 1)`, saturating
(an offset past the track's start clamps to group 0 either way). Older
drafts only encode a relative 0, so there it is SUBSCRIBE plus a relative
joining FETCH n groups back, as the live join already does. That filter
has no end, so a range with a bounded end resolves both bounds against the
Largest Object and goes upstream as an absolute range. Live keeps today's
join, `NextObject` plus a `Relative(1)` fill on draft-20+ or a relative
joining FETCH on older drafts (`ietf::subscriber::subscribe_join`), so the
current group's existing prefix still arrives. A relative end resolves
against the latest group the answering hop knows. Behind an IETF upstream
that is the Largest Object's group, so a newest group with no objects yet is
not counted until its first object arrives; the relay reports what it has
rather than waiting for the peer.

Today a track copy received over lite-06 or earlier, or any moq-transport
draft, refuses relative and time anchors with `Unsupported`, while a lite-07
copy resolves them locally (it learns the largest and media time and asks for
everything the budget allows; `lite::Version::resolves_anchors`). Each mapping
above that makes an older wire carry an anchor lifts that wire's refusal for
that anchor only, and extends the per-wire table in
`rs/moq-net/tests/integration/range_anchors.rs`.

Test the lite-05 relay case #5000 left out, on mock time beside
`rs/moq-net/tests/integration/late_lower_group.rs`: a live subscriber already reading
group 1, then one from `Absolute(0)`, and a fresh group 0 reaches only the
second. Today the merged request goes upstream as its covering span, which on
lite-05 has no floor and joins at the latest group, so group 0 never arrives;
it needs the explicit group 0 above. The merged-range relay test,
`rs/moq-net/tests/integration/subscribe_ranges.rs`, extends to lite-07 once it carries the
list.

Update `drafts/draft-lcurley-moq-lite.md` in the same PR: the SUBSCRIBE and
SUBSCRIBE_UPDATE fields, the Group Order section (the order is now a field),
the removal of FETCH, and the lite-07 changelog. This quest also owns the Order row of
the priority table in `doc/concept/moq-lite.md`: it now describes the field
(decided 2026-10-08, since subscribe ranges owns group order). Published versions (lite-03..06) keep
FETCH, answered through the model's ranges. Run `just drafts check` and
`just test interop --all`.

Decided 2026-10-22 (maintainer): SUBSCRIBE_OK carries the resolved start for
a relative range start, so a reader's ordered cursor takes its floor from it
([Container reads drop backfill](/quest/m1/read-backfill.md)).

## Required

- [Dynamic range requests](/quest/m1/subscribe-ranges/dynamic.md) - the misses a relay fills upstream by range

## Related

- [Container reads drop backfill](/quest/m1/read-backfill.md) - floors its ordered cursor on the resolved start
