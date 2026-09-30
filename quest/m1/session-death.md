# [S] Session death parity

## Goal

Rust and JS end a session's tracks and groups the same way:

- A session closed locally, on purpose, ends the tracks it was receiving
  cleanly in both languages. Groups still in flight end as they do today.
- A session that dies (peer close, transport failure) ends its tracks and
  its group readers with the session's error, carrying the peer's code,
  never the raw transport error, `Dropped`, or `Cancel`.

## Plan

https://github.com/moq-dev/moq/pull/4120 made a dying session end its tracks
with the session's error in both languages, and left two gaps.

Since then #4351 and #4378 changed Rust abort semantics: an abort keeps the
finished groups and drops only the open ones, so readers get what finished
and then the abort, or a clean end once the declared end settled. #4385 (open)
mirrors that in JS. Build the clean end below on those semantics.

Decided by the maintainer:

- **A local close is a close, not an error.** JS already ends tracks cleanly
  on `close()`. Rust has no clean end short of a finished track, so #4120
  ends them with the close error (previously `Cancel` or `Dropped`). Rust
  needs a clean end at the current edge that is not a declared end: readers
  get what was delivered and then `None`. Keep #4120's rule that an abort
  before a declared end settles still wins, and #4116's clean end for a
  dropped producer after `finish_at`; the new path must not mask either.
  Whether this is a new `track::Producer` method or a driver-internal path
  is an API call, so keep it private unless a consumer needs it.
- **JS group readers see the session's error.** Tracks go through
  `sessionCause` in `js/net/src/error.ts`, but `runGroup` in the lite and
  IETF subscribers closes a group with the raw error. Route it the same way.

Extend the existing session-death tests (Rust
`a_session_death_ends_the_track_with_its_error`, the JS lite and IETF
integration cases) with a local close and with a group reader, on lite and
IETF. Behavior change, no signature change on either side unless the Rust
clean end needs a public method.
