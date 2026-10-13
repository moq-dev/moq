# [S] Pin which caches count as a live feed on main

## Goal

On `main`, a draft-20+ FETCH takes the cache's newest object as Largest
Object only when the cache is fed live. A cache only fetches have filled,
never accepted, is not live, and neither is a parked warm cache with no copy
spliced after it. Tests pin both on `main`, and fix it there if either is
misclassified.

## Plan

The `release` backport ([#5402](https://github.com/moq-dev/moq/pull/5402))
treated both as live, so a FETCH past the stale edge was refused
INVALID_RANGE or capped. It was fixed there in `track::Consumer::is_live` and
`resume::Consumer::is_live`, with `a_fetch_only_cache_is_not_live` and
`a_parked_warm_cache_is_not_live`. A review said `main`'s `Feed::Live` check
already tells them apart; nobody has verified that.

Mirror those two tests against `main`'s live-feed check
(`track::Consumer::is_live` and its routes). If both pass, landing the tests
completes the quest. If either fails, fix the check at the source in the same
PR.

Public API: none. Wire: behavior only, if a fix is needed.

## Related

- [Draft-20 FETCH edge cases](/quest/m1/fetch-edge-cases.md) - the other follow-up from the same backport review
