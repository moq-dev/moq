# [S] An evicted group is judged stale by its successor

## Goal

A group already handed to a reader stays judgeable after the cache evicts
it: once its successor's arrival plus the reader's budget has passed, the
group is stale, the same verdict `hooks.staleAt` already gives a resumed
copy's group by sequence alone. A serve loop that waits on such a group (a
group stream whose FIN never completes) is then reset as `Old`, whatever the
budget.

Today the verdict needs the group to still be cached. Rust `GroupExpiry`
(`rs/moq-net/src/model/track.rs`) requires the sequence in the cache lookup,
and JS `#isStale` (`js/net/src/track.ts`) requires the group in the
timeline. Both caches evict an idle group after their window (Rust
`cache::DEFAULT_EXPIRY`, JS `CACHE_WINDOW`, both 30 s), so under a budget of
about 30 s or more a stalled group is never convicted. Since
[#6059](https://github.com/moq-dev/moq/pull/6059) the same stalled stream
also holds the IETF PUBLISH_DONE, which waits on a finished group's FIN
under the budget.

## Plan

Found in #6059's follow-ups and decided with the maintainer on 2026-10-20 as
a sibling of [Drop set](/quest/m1/drop-set.md): that quest judges missing
groups, this one judges groups a reader already holds.

- Judge by the successor, as `#staleAt` does for a sequence the track does
  not hold. The verdict needs the successor's arrival (and its reach, for the
  media-time arm), not the evicted group itself.
- Find out what the cache-membership checks guard before dropping them, such
  as a different copy now occupying the sequence, and keep that guard.
- Settle what the verdict reads once the successor is evicted too. It must
  never convict early: any later group arrived no earlier than the successor,
  and the newest group is never stale.
- Check `FetchExpiry` beside `GroupExpiry`, which has the same cache-lookup
  condition.

Regression tests on mock time, Rust and JS, each failing without the fix,
with a budget of at least the cache window: a handed-out group whose stream
never completes is evicted, then reset as `Old` once its successor's arrival
plus the budget passes, and on IETF the PUBLISH_DONE then goes out. The
newest group is still never convicted.

Public API: none. Wire: none.

## Related

- [Drop set](/quest/m1/drop-set.md) - each cursor judges missing groups against its own budget
- [#6059](https://github.com/moq-dev/moq/pull/6059) - bounds a stalled group stream's FIN wait by the age budget, which this extends past eviction
