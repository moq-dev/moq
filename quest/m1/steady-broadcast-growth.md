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

Profiled 2026-11-25 on `b367c92954` (draft
[moq-dev/moq#9579](https://github.com/moq-dev/moq/pull/9579)): the passthrough
chain on loopback, a 320x240 60 fps source with every frame a keyframe (60
groups/s), jemalloc `lg_prof_sample:17`, dumps every 5 min over 20 min. RSS
stayed in noise at that length, but the heap diffs showed one growth in all
three processes, one entry per group in each live reader:

- `Record::reported` (`rs/moq-net/src/model/record.rs`, via
  `DropCursor::next` → `Record::report`): import +2.1 MB, export +1.7 MB,
  relay +1.7 MB over 15 min.
- `Misses::given_up` (`rs/moq-net/src/model/track/miss.rs`
  `adopt_drops`): relay +1.3 MB, import +0.9 MB.

Cause: every `append_group` (and on a received copy, every GROUP_END's
`skip_groups`) skips the keys between consecutive group starts, each reader
reports that skip, and the record folds only under pool debt or when its
ranges move. A stock pool is far below its target, so nothing folds. A unit
test (`a_sparse_live_track_keeps_a_bounded_record`) measured 3,999 billed runs
after 2,000 groups.

Fix (on the draft): `Fold::Final`, a lossless fold that `Record::settle`
runs whenever the record has doubled since its last fold. It folds only
delivered keys and `Skip`/`Settled` reports. It stops at `Dropped` reports
(a skip may still replace them), stale skips, written-off runs, `Misses`'
stale verdicts (`Beside::next_stale`), pending runs, and the group being
handed out. `Misses`' given-up runs fold with it through `Beside::forget`.
The test now peaks at 3 runs.

Remaining:

- Run the full `moq-net` suite (`just rs test -p moq-net --features async`)
  and `/verify-subscribe` on the fix; neither ran yet.
- Re-profile with the fix over a longer run (1 h or more) to confirm all
  three level off. The QUIC recv leases (`Leased::fill`) and cached
  groups/frames swung between dumps and looked like in-flight data, but
  confirm they don't trend.
- The Nightly steady run from the Goal: the repro at
  `.tmp/steady/repro.sh` in the draft's worktree is the shape (a live
  ffmpeg source at a high group rate | `import ts --passthrough` → relay →
  `export ts --passthrough`, sampling `/proc/<pid>/status` VmRSS and
  jemalloc heap dumps). Build it on `test/lib/harness.sh`, judge the slope
  after warm-up, size the run by seeding a known leak (revert this fix), and
  wire it into `.github/workflows/nightly.yml`.
- Ask the reporter to run the fix on their lane once it lands.

Public API: none expected. Wire: none.

## Closes

- [#9539](https://github.com/moq-dev/moq/issues/9539) - `moq-relay` and `moq export ts --passthrough` keep growing over 72 h on one 25 Mb/s passthrough broadcast

## Related

- [A nightly soak shows a churning relay stops growing](/quest/m4/relay-soak.md) - the churn case, which reuses this quest's slope judge
