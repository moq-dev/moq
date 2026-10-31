# [S] A kept draft-18 join pins its head group

## Goal

On draft 18, a joining FETCH that our publisher keeps open after a quick
finish is answered even if the publisher drops its `track::Producer` right
after `finish()`: the kept join pins the head group in the cache until it is
answered, instead of the late FETCH being refused as not found. The
draft-18 interop `tail` and `finish` cells pass with a Rust publisher.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7306](https://github.com/moq-dev/moq/pull/7306): m2, as
recommended. Start after [moq-dev/moq#7306](https://github.com/moq-dev/moq/pull/7306) (open) lands; it owns
[the join after finish](/quest/m2/ietf-join-after-finish.md).

[moq-dev/moq#7306](https://github.com/moq-dev/moq/pull/7306) keeps the subscription's request open after PUBLISH_DONE but does not
hold the head: when the producer drops, the cache demotes the latest group,
and the late FETCH gets a loud refusal (accepted by that quest's Plan).

- Hold the head group from the retained subscription (a cache pin or a
  held `group::Consumer`) until the join is answered or the request closes,
  bounded by the request's lifetime, so no new constant is needed.
- Test: finish then drop the producer, then a late joining FETCH on draft
  18 still gets the head.
- Rerun the draft-18 interop `tail` and `finish` cells with the Rust
  publisher, as [IETF finish](/quest/m2/ietf-finish.md) asks.

Public API: none. Wire: none.

## Related

- [A quick finish keeps its pre-draft-22 join](/quest/m2/ietf-join-after-finish.md) - [moq-dev/moq#7306](https://github.com/moq-dev/moq/pull/7306) lands the retained join
- [IETF finish](/quest/m2/ietf-finish.md) - owns the interop cells
