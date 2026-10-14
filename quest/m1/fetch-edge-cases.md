# [S] Backport draft-20 FETCH edge cases to 0.17

## Goal

After the main fixes have merged, backport the two draft-20+ single-group
FETCH edge cases to `release` in a later 0.17 patch, preserving its published
API and carrying regression tests:

- A range ending exactly at the final object of a finished track answers
  FETCH_OK with End of Track set, so a relay handling it finishes its copy.
- A FETCH_OK whose End Location is past the requested group closes the
  session with PROTOCOL_VIOLATION instead of being accepted, and never marks
  that later group as the track's end.

## Plan

Found by Codex on the release backport
([#5402](https://github.com/moq-dev/moq/pull/5402#discussion_r4260963177) and
[#5402](https://github.com/moq-dev/moq/pull/5402#discussion_r4260963190)); the
same code is on `main`.

The main fixes and regression tests landed in
[#5487](https://github.com/moq-dev/moq/pull/5487). Port them in a separate
backport PR targeting `release`:

- Publisher: a bounded FETCH reaches the end only if the group is finished
  and the read cursor reached or passed its final frame count. A start past
  the final object must still be refused INVALID_RANGE on draft 20+.
- Subscriber: validate End Location stays within the requested group before
  accepting the group or applying End of Track.

The regression tests cover drafts 20 through 22, including a shorter-prefix
control and rejecting malformed responses both with and without End of Track.

Decided 2026-10-13 (maintainer): this does not gate the next 0.17 cut on
[the release line](/quest/m0/release-22/README.md); the backport ships in a
later 0.17 patch.

Public API: none. Wire: behavior only, within the existing draft-20 through
draft-22 FETCH form.

## Related

- [Main live-feed check](/quest/m1/fetch-live-feed.md) - the other follow-up from the same backport review
