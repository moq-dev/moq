# [M] No resume or relay route mutant survives

## Goal

Nightly's mutants shards report no survivors in
`rs/moq-net/src/model/resume.rs` or `rs/moq-net/src/route/relay.rs`.

## Plan

[moq-dev/moq#9750](https://github.com/moq-dev/moq/pull/9750) killed or excluded every
survivor from Nightly runs [45667362304](https://github.com/moq-dev/moq/actions/runs/45667362304)
and [45867457463](https://github.com/moq-dev/moq/actions/runs/45867457463) but one:

- `Reader::backfill_gaps` match guard `partial` to `true` (the `Some(at) if partial => Some(at)`
  arm that sets `copy.drains`). Under the mutant, a whole-route walk keeps a stale eviction
  count, so `lost` (and a let-go of a draining copy) fires when the original wouldn't. That only
  adds a whole-route walk. #9750 made a let-go of a draining copy re-arm the walk, which removed
  the one case where the extra walk changed what was delivered. Cases still not covered: a left
  copy can stop draining with no re-arm until it is let go, when it expires by budget, refuses
  the group as stale while a read keeps it, `may_take` turns false, or holds the group without its
  head. In each of these an extra walk only changes timing, but nobody has proven that.
  Recommended fix: store each copy's left keys and recheck the drain predicate for them every
  poll, instead of keeping an eviction count that a walk has to refresh. The walk then fires
  exactly when a left key stops draining, and the guard goes away. Run `verify-resume` after
  (`ResumeReader.tla` `LetGo`/`Pass` model the re-arm).

Public API: none. Wire: none.
