# [S] A FETCH on release never stalls on a parked open group

## Goal

On the 0.17 `release` line, a FETCH of a group that a live subscription
delivered only in part, after that subscription ended, receives every frame
the publisher writes and ends with the group. Release only: `main` deleted the
warm copy in #4741 and is unaffected. Lands with the next scheduled 0.17.x; no
dedicated cut.

## Plan

Reported in #5348 and bisected to b425d430f (the #4922 backport). Traced
2026-10-12; the pin check the commit message mentions is not the cause.

- Mechanism: the backport puts every viewer session on one front, so viewer
  B's FETCH joins viewer A's parked front. When A's subscription ends
  mid-group, `Action::Park` runs `warm_copy`, and `warm_rebuild` copies the
  open live-edge group into a new group that it only finishes if the live one
  finished. The upstream subscription is already cancelled, so nothing ever
  writes to that group, and it is not aborted until `TRACK_IDLE_LINGER`.
  `resume::Consumer::cached_group` then reaches `track::covering_group`, which
  accepts any group that isn't aborted, finished or not. The FETCH gets the
  cached frames and then waits forever.
- Decided 2026-10-12: an unfinished group in a warm segment is a cache miss
  for FETCH. `cached_group` (`model/resume.rs`) skips it, and
  `Fetching::poll_latch` does not latch onto a warm newest segment. The
  pending fetch then marks the track used, the front re-splices, and the
  FETCH goes upstream on the new copy (`pending_fetch_follows_a_newer_segment`
  already covers that last hop). Rejected: a spliced `resume::Group` that
  continues the open warm group, since it is more code on a line `main` already
  deleted; and aborting open groups at park, since it breaks a returning
  subscriber continuing an open warm group.
- Regression in release `model/origin.rs` tests, modelled on
  `returning_reader_continues_an_open_warm_group` with `served_front()`:
  subscribe, write two frames into open group 0, read one, drop the
  subscriber and wait for the park, then `fetch_group(0, None)`. Expect a new
  upstream track request that serves the whole group, and the fetch ends with
  every frame.
- The PR targets `release` directly (CONTRIBUTING's backport flow), since
  there is nothing to land on `main` first.

Public API: none. Wire: none.

## Closes

- [#5348](https://github.com/moq-dev/moq/issues/5348) - FETCH of a partly delivered group stalls on 0.17.3
