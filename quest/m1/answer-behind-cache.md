# [S] A route answer behind its idle copy's cache fails the copy

## Goal

When an idle track copy resumes and its route's answer (lite SUBSCRIBE_START
or moq-transport SUBSCRIBE_OK) is behind what the copy already cached from
that path, the copy fails its track instead of guessing. Behind means no
Largest while the copy holds shown groups, or a Largest position before the
newest position the copy holds. Readers get the error, the lingering copy goes
away, and the next subscribe builds a fresh copy that accepts the feed from
group 0. Today the no-Largest case hides the cache behind a live floor while
`claim_sequence` still refuses the feed's re-sent groups as `Duplicate`, so
live readers stall until the feed passes the old newest group, forever for a
one-group catalog.

Non-goals: detecting a genuine publisher restart on the same path. Restarted
numbering needs a new broadcast name
([moq-dev/moq#4741](https://github.com/moq-dev/moq/pull/4741)). Rust only:
`js/net` has no idle-copy reuse.

## Plan

Planned 2026-11-06 (maintainer, /quest-import of
[moq-dev/moq#7973](https://github.com/moq-dev/moq/issues/7973)). Reproduced on
main with the issue's test: the first re-sent group fails with
`Err(Duplicate)`. `release` predates the disowning arm, so nothing to
backport.

Decisions:

- Direction: ✅ fix in moq-net / won't fix / fix in moqxr only.
- Shape: max edge (delete the disowning arm and trust the cache) / ✅ error
  on any disagreement / error only on a missing Largest. The maintainer
  first picked the max edge, then chose the error: a cache that disagrees
  with its route is likely the wrong track, and failing loud makes a
  non-compliant upstream (moqxr omitting Largest while it has content) fix
  it rather than the relay serving stale groups. The max edge would also be
  inconsistent, since reusing a broadcast does not always answer below the
  cache.
- Milestone: ✅ m1 / m0. Additive, no public API or wire change.

Recommendations:

- `answered` (`rs/moq-net/src/model/track.rs`) judges the disagreement
  against the same shown-groups view it uses now, which already discounts
  the group the idling cancel reset. The `(None, Some(cached))` live-floor
  arm goes away; keep the gap arm (a start past the cache), which is not a
  disagreement.
- Fail with the existing `Error::MalformedTrack`: it maps to the
  stream-scoped `MALFORMED_TRACK` code, so the upstream session survives.
  `Duplicate` would map to a session-level protocol violation.
- Make `set_live`/`set_live_ahead` report the failure to their callers in
  `lite/subscriber.rs` and `ietf/subscriber.rs` rather than aborting inside
  the model, if that keeps the copy's teardown in one place.
- Tests on the model: both disagreements fail readers; the issue's restart
  sequence succeeds on the fresh copy;
  `a_relative_start_on_an_idle_copy_skips_a_cache_the_route_disowns` is
  replaced. One session-level test that a no-Largest SUBSCRIBE_OK over a
  lingering copy fails the downstream subscription and a resubscribe reads
  group 0.

## Closes

- [#7973](https://github.com/moq-dev/moq/issues/7973) - a lingering track refuses a restarted publisher's groups as duplicates
