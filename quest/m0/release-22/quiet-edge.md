# [S] A quiet track's latest group survives rejoin on the 0.17 release line

## Goal

On a 0.17.x moq-relay, a late subscriber to a live but quiet track (such as
a catalog published once) receives its latest group after earlier readers
left and rejoined and the cache expiry passed, including across a publisher
takeover.

## Plan

Reported in [#5413](https://github.com/moq-dev/moq/issues/5413) against
moq-relay 0.17.3 / moq-net 0.3.11. Verified 2026-10-13: the reporter's
`rejoined_quiet_catalog_survives_cache_expiry` test fails on the
`moq-net-v0.3.11` tag and its port passes on `main`, where #4741 deleted the
warm-copy code (`WarmGroup`, `WarmCopy`, `TrackIo`) and readers read the
serving route's copy directly.

Root cause on `release` (`rs/moq-net/src/model/origin.rs`): the cache exempts
only a live track's latest group from expiry. On re-splice, dropping the
`WarmCopy` finishes its track, so its latest group is demoted while
`io.head` still holds it as the live edge. The new upstream copy resumes
after it, so nothing re-sends it, and the pool expires it. `TrackIo::retire`
returns early on a finished held group, so takeover loses it the same way.

Decided 2026-10-13 (maintainer):

- Backport to `release` only; `main` gets [a regression test](/quest/m1/quiet-edge-test.md).
- Keep the `WarmCopy` alive and unfinished until the new copy delivers a
  group newer than its edge, so the one existing rule (a live track's
  latest is protected) covers it. Give `retire` the same treatment for a
  finished held group. Preferred over the reporter's patch (`WarmGroup`
  holding a `cache::Protection` released by `Step::Held`), which adds a
  second protection path to keep in sync. Fall back to it only if keeping
  the copy alive proves invasive.
- Land the reporter's test (Lite05/06, superseded group still expires) and a
  takeover variant.
- Ships with the next 0.17 patch alongside [draft-20+ FETCH](/quest/m0/release-22/fetch.md).

## Closes

- [#5413](https://github.com/moq-dev/moq/issues/5413) - quiet catalog lost after reader rejoin and cache expiry
