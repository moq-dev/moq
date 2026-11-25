# [L] One steady broadcast stops growing the relay and the TS passthrough

## Goal

On one steady live broadcast, `moq-relay`, `moq import ts --passthrough` and
`moq export ts --passthrough` level off after warm-up instead of growing for
days. Each growth is found with a heap profile, fixed at its cause, and a
Nightly steady-state run fails if it comes back. Non-goal: growth under
session and broadcast churn, which [the relay soak](/quest/m4/relay-soak.md)
owns.

## Plan

Reported in [moq-dev/moq#9539](https://github.com/moq-dev/moq/issues/9539):
one 25 Mb/s passthrough chain on loopback over moq-lite-07, default relay
cache settings, grew about linearly for 72 h on `main` `9df4313a`. The relay
grew 257 → 444 MB (+2.37 MB/h, R² 0.94, with the last day the fastest), the
export +1.66 MB/h and the import +0.94 MB/h. The clock steps the passthrough
splices didn't move it. The reporter is taking a 24 h jemalloc heap profile
(`moq-relay/jemalloc`, `moq-cli/jemalloc`, dumps on `SIGUSR1`) and offered
more runs.

Decided 2026-11-24 (maintainer, /quest-import):

- ✅ Profile it ourselves rather than wait for the reporter's profile
  (rejected: waiting on their profile; folding it into the relay soak, which
  leaves the leak unfixed until the soak exists). Compare with theirs once
  it's posted.
- ✅ m1: a fix in shipped code that needs no breaking release, for growth
  without bound on any long-lived broadcast.
- ✅ This quest adds a Nightly steady run (rejected: a unit test only, blind
  to the next leak on the path; both, more than an m1 fix needs). It runs the
  passthrough chain (import → relay → export) at a high group rate, so hours
  of groups fit in minutes, and fails on a resident or heap slope after
  warm-up, naming the process that grew. Measure the run length by seeding a
  known leak, as the relay soak plans, and build the slope judge so the
  relay soak can reuse it for churn.

Leads from reading the code at `f031ffec29`, unconfirmed until profiled.
Passthrough cuts a group at each video random access point, or after 1 s
(`rs/moq-mux/src/container/ts/passthrough.rs` `FALLBACK_TICKS`), so thousands
of groups an hour, keyed by their start time in microseconds
(`Track::append_group`). The relay's slope is then a few hundred bytes per
group.

- A reader's `Record` (`rs/moq-net/src/model/record.rs`) keeps runs of keys
  handed out, skipped, pending and reported, and folds them below `floor` only
  under pool pressure. A stock relay's headroom governor seldom reaches that
  pressure, and with sparse keys a run that `forget_past_budget` can't merge
  stays its own entry. This is the likeliest lead.
- Each `append_group` records the keys skipped since the previous group in
  the track's drop set, which `expire_drops` forgets only below the live edge.
- The track and session arrival ledgers (`ledger.rs`, `tail.rs`) open a gap
  per sparse key, closed after the max delay.
- `moq-quic` keeps each send stream's end state until `release()`. A path
  that bypasses the handle that releases it on drop would keep one per
  stream.

A quick discriminator: if the pool's billed record bytes climb, it's one of
the first three, and `--cache-capacity` should flatten the relay. The import
and export have no byte capacity at all, so they would show the same shape.
Ask the reporter to run the fix on their lane, but don't wait on it to land.

Public API: none expected. Wire: none.

## Closes

- [#9539](https://github.com/moq-dev/moq/issues/9539) - `moq-relay` and `moq export ts --passthrough` keep growing over 72 h on one 25 Mb/s passthrough broadcast

## Related

- [A nightly soak shows a churning relay stops growing](/quest/m4/relay-soak.md) - the churn case, which reuses this quest's slope judge
