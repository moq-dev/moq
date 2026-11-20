# [M] A lite-07 answer's SYNCED holds across grant changes

## Goal

A lite-07 publisher (Rust `lite/publisher/routed.rs`, JS `js/net/src/lite/answer.ts`)
sends a prefix's SYNCED only once every claim the grant in effect shows under it has
had its Start, however grant changes (widen, narrow, move, revoke), claims, mounts, and
the peer's edits interleave with the output. A claim that leaves only because of the
permit ends as lost, never as withdrawn. A claim is shown only where its own reach,
the handle's scope, and the permit share a path. A property test in each language
checks these over random sequences and stays fast enough for `just check`.

## Plan

Found by CodeRabbit on [moq-dev/moq#9202](https://github.com/moq-dev/moq/pull/9202),
whose answer this fixes: each review round there found another ordering, so this quest
replaces case-by-case fixes with a property test.

- ✅ Fold [the claim-reach quest](/quest/m2/lite07-claim-reach.md) in (rejected: keep
  it separate): the same `shows` the property exercises, and its tests share the
  harness.
- The property's oracle is differential: when SYNCED goes out, a fresh answer on the same
  origin, under the grant in effect in stream order (the last grant marker the answer
  processed), asked for the same prefix and drained, names the claims required. Only those
  present since the latest read for the prefix started (its edit or that grant change)
  are required, since a claim arriving after its read's initial set is live.
- Rust carries each claim's reach (`origin::RouteEntry::reach`, translated through a
  mount) to `Answer::shows`, looked up in the origin when the node tells the claim, and
  intersects it with the permit beneath the presented path before the handle check. JS
  applies the claim and handle scope intersections with or without a grant.
- Settled on #9202, not reopened: answering region by region (option D); a synced reader
  goes unsynced only when a widening can show it more; a claim is shown when the scope
  matches some path it covers; replacement mounts hold SYNCED until their initial set
  arrives.

Public API: none. Wire: none.
