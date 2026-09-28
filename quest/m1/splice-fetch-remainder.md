# [S] A spliced group fetches its remainder

## Goal

An origin-routed reader never parks forever on a half-delivered group. When a
publisher aborts a track mid-group and re-creates it under the same name, the
reader today waits in `read_frame` with no error while a direct reader gets
the abort. After the fix the half-delivered group either completes or ends
with an error, and the reader continues on the new track from its next group.

## Plan

Decided: a same-name re-create is a continuation, not new content, for local
and remote sources alike. No special case for local broadcasts.

Why it hangs: `track_ended` in `rs/moq-net/src/model/front.rs` treats an abort
after delivery as failover and re-splices from the serving source at the first
missing frame. The resumed group then waits in `poll_peek_group`
(`rs/moq-net/src/model/resume.rs`) on the new copy. A copy only answers
"never" for a group below its declared start, and an in-process producer
declares none, so "not yet" lasts forever. The expiry guard is attached only
once a copy is found.

Decided: once the new copy is past the missing group, the resumed group FETCHes
the remainder (`frame_start` at the missing frame) instead of peeking.
`fetch_group` always answers: a local producer with no fetch handler says
`NotFound` at once, so the group ends with that error, and a remote copy can
serve the rest from the upstream cache. Keep peeking while the live route may
still deliver the group, since that is what avoids a redundant fetch.

Regression test: the issue's repro (abort mid-group 2, re-create at group 3,
through the origin) on a paused clock; group 2 ends with an error and group 3
arrives.

## Closes

- [#4365](https://github.com/moq-dev/moq/issues/4365) - close this issue when the quest finishes

## Related

- [Splice edge cases](/quest/m1/splice-edges.md) - the same splice code and test harness
