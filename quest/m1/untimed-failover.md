# [S] Give up a resumed group drift can't judge

## Goal

After a route failover, a resumed group that no remaining route continues is
given up even when its track's media time can't measure how far behind it is,
such as on an all-untimed track. Today a reader holding that group waits until
the track ends.

## Plan

Found in #4822. `Recover::poll` (`rs/moq-net/src/model/resume.rs`, around line
854 at time of writing) gives up a group the serving route can't continue
once `poll_stale` reports drift past the reader's `max_age`. `drifted`
(`model/track.rs`) needs a timed live edge and a timed successor (`reach`).
Without both, nothing convicts the group. Only the holder of that
`group::Consumer` blocks: track cursors keep handing out newer groups. The
replaced copy's lease also stays alive. On main this still worked, because
receivers stamped arrival time on untimed frames.

Decided 2026-10-05 11:39 +0200:

- A separate quest, not a fix inside #4822, to keep that PR reviewable. It
  ships the regression for a short window, which is why this quest is ranked
  right after it.
- The rule: when drift can't be measured (no timed live edge, or the
  successor is untimed), give up once the serving copy holds a newer group
  and no route or recovery fetch can still fill this one. This applies to
  mixed tracks too, so there is one rule. No clock is involved.
  Rejected: a wall-clock deadline of `max_age` after the stall, which stands
  wall time in for media time, the substitution the untimed model removed.
  Also rejected: leaving it and documenting it.

Settle what "nothing pending" means against the recovery fetch
(`a_pending_recovery_fetch_does_not_disable_expiry`), so a fetch about to
fill the group isn't cut short. A FETCH reader has no budget and stays as it
is.

Test: a model test in `resume.rs` failing over an all-untimed track with an
abandoned group, plus one where the successor is untimed on a timed track.
Both fail without the fix. Run each against a cold route and against one
whose copy already caches untimed groups past the resumed one, so a cursor
that skips ahead can't hide the stall. #4822 is changing `Cursor::new` so an
explicit start is honoured on untimed tracks; build on that, not on the
older jump to the latest untimed group.

Public API: none. Wire: none.

## Required

- [Untimed model](/quest/m1/untimed-model.md) - introduces untimed groups, and the stall

## Related

- [JS track handover](/quest/m1/js-group-handover.md) - mirrors this rule in JS
- [Wall-clock age-out](/quest/m1/cache-wall-eviction.md) - retention of untimed groups, not a blocked reader
