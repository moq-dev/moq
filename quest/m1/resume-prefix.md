# [M] Keep a resumed group's head across a route change

## Goal

When a front resumes a group on another route of the same epoch, the
replacement copy adopts frames 0..N-1 the replaced route already received and
fetches only N onward. The replacement route's cached group is then whole, so
a later reader or fetch of it from frame 0 is a cache hit, the same as on a
relay that never saw the route change. This holds for every group, compressed
or not, and for a replaced copy that failed as well as one that stalled.

Boundaries:

- A plain (non-front) consumer of an aborted group still pins nothing: the
  abort releases its frames, and only readers that can resume keep the head
  until they hand it over.
- Cache policy (expiry, eviction, too old) still frees the frames it drops;
  only a copy that failed or was given up keeps its head for a successor.
- Not compression. [Compressed tracks](/quest/m1/compress/core.md) builds on
  the adopted head to rebuild a group's inflater; this quest only names the
  hook.

## Plan

Decided with the maintainer: frames 0..N-1 survive a route change for every
group; compression is not special.

- A group handed out from a front's logical track holds a shared hand-off
  for its copy. Aborting that copy (other than by cache policy), or dropping
  it unfinished, moves the committed frames into the hand-off instead of
  dropping them; a plain consumer still reads the abort, never the frames.
  The hand-off is freed with the last reader holding it, so nothing outlives
  the readers that could use it.
- On resume, the reader adopts the head into the replacement group before
  reading from it: frames below where that group starts, from the stalled
  copy's own cache or from the hand-off. The fetch still asks from the
  reader's frame, so no wire, handler, or request shape changes.
- Adoption bills the replacement's cache, refuses an overflow by skipping
  the adoption, and keeps IETF object fields and the first-object marker
  aligned. Sparse groups already refuse ordinal recovery and stay that way.
- js/net has no route resume on main ([JS track handover](/quest/m1/js-group-handover.md)
  ports it), so there is nothing to mirror yet; that quest mirrors this
  contract.

Tests: a failed and a stalled route both leave the replacement's cache whole,
so a later fetch from frame 0 is a hit; a plain consumer of the aborted group
still reads the abort with nothing pinned; a cache-policy abort keeps
nothing. Mocked time, no sleeps.

Public API: none. Wire: none.

## Related

- [Compressed tracks in Rust moq-net](/quest/m1/compress/core.md) - rebuilds a resumed group's inflater from the adopted head
- [JS track handover](/quest/m1/js-group-handover.md) - mirrors route resume, including this, in js/net
