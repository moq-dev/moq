# [S] A resume more than one group behind never hangs

## Goal

A resumed read that starts more than one group behind the latest one gets
every group or a drop for it on every wire version, never a hang. The fix is
the simplest one that works; moq-transport FETCH/SUBSCRIBE edge cases are not
worth draft-specific fallbacks.

## Plan

Planned 2026-11-02 (maintainer, follow-up planning after the 2026-11-02 /quest-complete run), from [moq-dev/moq#7631](https://github.com/moq-dev/moq/pull/7631), which
found it and kept its two direct bounded-resume tests on draft 22 only.

- It hangs on `main` too, even with an open-ended range, over a draft-18
  upstream. Two causes: our moq-transport publisher answers only one-group
  FETCHes, so the fill for the missing run never arrives (a one-group range
  works); and the live join can skip a group that is already cached
  (`TrackState::answered` in `rs/moq-net/src/model/track.rs`).
- Also check whether the moq-lite publisher has the same wait when it serves a
  bounded SUBSCRIBE from a moq-transport copy that was only fetched.
- Multi-group past reads between our own moq-transport nodes are refused until
  hole markers land (decided on
  [moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625)); a refused run
  must come back as dropped, not hang.
- Enable the draft-18 cases of #7631's bounded-resume tests as the regression.
- A relay loses a group on a history SUBSCRIBE (folded in 2026-11-02,
  maintainer, from [moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625)):
  with the `fetched_copy` fixture in `fetch_only` on draft-22, where the relay
  already fetched and cached group 2, a fresh session's `1..3` read gets only
  group 2. The relay asks upstream for groups 1 to 2 and upstream sends only
  group 2, so group 1 is lost silently (`main` hangs instead). Group 1 must
  arrive or come back as dropped. The `1..2` case #7631 tests is fine.
