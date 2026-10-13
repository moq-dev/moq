# [S] Draft-20 FETCH edge cases, main first

## Goal

Two edge cases in the draft-20+ single-group FETCH from
[#4971](https://github.com/moq-dev/moq/pull/4971) are fixed on `main`, each
with a regression test that fails without its fix, and then backported to
`release` in a later 0.17 patch:

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

- Publisher (`rs/moq-net/src/ietf/publisher.rs`): `read_fetch` stops at
  `until` without seeing the group's end, so `group.complete` stays false.
  Derive End of Track from the track's final sequence and the group's last
  object rather than from having read the group's end.
- Subscriber (`rs/moq-net/src/ietf/subscriber.rs`, `run_group_fetch`): the
  draft-20 check refuses an End Location before the start only. Also refuse
  one whose group differs from the requested group.

Decided 2026-10-13 (maintainer): this does not gate the next 0.17 cut on
[the release line](/quest/m0/release-22/README.md); the backport ships in a
later 0.17 patch.

Public API: none. Wire: behavior only, within the existing draft-20 through
draft-22 FETCH form.

## Related

- [Main live-feed check](/quest/m1/fetch-live-feed.md) - the other follow-up from the same backport review
