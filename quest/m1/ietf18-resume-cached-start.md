# [M] A draft-18 resume of a cached group under a finite max-age settles

## Goal

On moq-transport draft-18, a track copy that went idle holding a group
resumes a bounded subscription of that group after the upstream moved on.
When the publisher's max-age is finite, the read either delivers the group,
as moq-lite and drafts 20 and 22 do, or ends at once with a refusal the
reader can see. It never waits forever for a group that won't come. Done when
`a_resumed_bounded_subscription_keeps_its_cached_start` runs every version
with an explicit finite max-age (30 s) and asserts each version's outcome,
with no draft-18-specific fallback added.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`), from the follow-ups of
[moq-dev/moq#7729](https://github.com/moq-dev/moq/pull/7729). The problem
predates that PR, since media tracks were already 30 s. It surfaced when
raw tracks became finite.

The read asks for group 0 below the copy's live floor. A finite budget can't
judge a group below that floor (`TrackState::cache_floor` in
`rs/moq-net/src/model/track.rs`), so the copy must fill it from upstream.
moq-lite and drafts 20 and 22 do that. Draft-18 doesn't, and the read never
gets group 0.

- Reproduce it with `a_resumed_bounded_subscription_keeps_its_cached_start`
  (`rs/moq-net/tests/integration/fetch_only.rs`). Give its track an explicit
  finite max-age, such as 30 s, rather than relying on a default. #7729 is
  changing the authored default to 0 s unless the publisher asks for more
  (maintainer, 2026-11-03), so a default would test a different case.
- Follow the standing rule for moq-transport FETCH versus SUBSCRIBE: pick the
  simplest path and accept a refusal on the awkward draft rather than add a
  draft-specific fallback. If draft-18 can fill the group through the path
  the other drafts already use, use it. Otherwise end the read with a
  refusal, and have the test assert that for draft-18, as the multi-group
  FETCH test beside it already does.
- moq-lite's behavior keeps full correctness. Don't change it to match.

Public API: none. Wire: none.

## Related

- [A kept draft-18 join pins its head group](/quest/m2/ietf-join-pin-head.md) - another draft-18 read that the publisher's lifecycle cuts short
