# [L] Lite-07 ranges

## Goal

moq-lite-07's SUBSCRIBE carries group ranges and an order, SUBSCRIBE_UPDATE
replaces them, and FETCH is gone from lite-07. The Rust publisher serves
ranges from the model and drops what it won't deliver, and a relay fills a
lite upstream by subscribing to the missing ranges.

## Plan

Update `drafts/draft-lcurley-moq-lite.md` in the same PR: the SUBSCRIBE and
SUBSCRIBE_UPDATE fields, the Group Order section (the order is now a field),
the removal of FETCH, and the lite-07 changelog. Fix the stale "offset by 1"
sentence on Group Start while there. This quest also owns the Order row of
the priority table in `doc/concept/moq-lite.md`: it now describes the field
(decided 2026-10-08, since subscribe ranges owns group order). Published versions (lite-03..06) keep
FETCH, answered through the model's ranges. Run `just drafts check` and
`just test interop --all`.

## Required

- [lite-07 Live flag](/quest/m1/lite-live.md) - reshapes the same SUBSCRIBE first
- [Model ranges](/quest/m1/subscribe-ranges/model.md) - the model this serves
- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - lite-07's DROP for holes
