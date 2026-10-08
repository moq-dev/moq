# [L] One max_age meaning for both languages' track caches

## Goal

`max_age` means the same thing in Rust and js/net, for both publisher
retention (`track::Info::max_age`) and a subscriber's budget
(`Subscription::max_age`). A group other than the newest is stale once either
its wall-clock age since its successor arrived at this hop, or its media-time
age (the live edge minus its reach), reaches the budget. The newest group is
never stale. An untimed group has no media time, so the wall clock alone
judges it.

That one rule also gives up a resumed group no route continues after a
failover, on untimed tracks too, and lets an untimed track's start replay
what isn't stale instead of jumping to the latest group. The pool's (Rust) or
cache window's (JS) wall-clock idle bound stays separate from `max_age`.

## Plan

Decided (2026-10-06, maintainer):

- Stale on either clock. The wall term bounds a peer that lies about PTS,
  which media time alone would never evict. The media term evicts earlier
  than the wall can, as when a startup burst arrives all at once. Rejected:
  stale only when both clocks agree, which keeps the lying-peer case.
- The wall clock starts when the group's successor arrives at this hop, the
  wall-clock twin of media time's reach (bounded by the immediate
  successor). A group with no successor never ages.
- "Successor" is the group `reach` uses: `first_servable` above it, so
  publisher-produced (not fetched backfill), not aborted, and below the
  subscriber's `set_groups` cap (retention has no cap). The clock is that
  group's arrival, re-read like reach rather than latched. If N+2 arrives
  before N+1, N's clock starts at N+2's arrival, and restarts at N+1's
  arrival once N+1 shows up. If the successor aborts, the clock falls to the
  next servable successor's arrival, or stops if there is none.
- Accepted: during a total upstream stall, a reader still on a superseded
  group is skipped forward after `max_age`, where media time alone would let
  it wait. That reader is `max_age` of real time behind live, and the newest
  group is never evicted.
- Applies to both retention and subscriber budgets, which share `is_stale`
  in `rs/moq-net/src/model/track.rs` today.
- Supersedes two earlier decisions: the untimed model's ([#4822](https://github.com/moq-dev/moq/pull/4822))
  2026-10-01 rejection of `max_age` on
  max(wall, pts), whose worry was a congestion stall (answered by starting the
  clock at the successor), and the retired untimed-failover quest's no-clock
  rule, which this rule replaces.
- Blocked readers need a deadline wake. `TrackState::poll_stale` re-checks
  only when the track state changes, and after a failover the successor may
  be the last change, so a lazily checked wall term never fires for a reader
  parked in `Recover::poll` or a subscriber waiting on a superseded group.
  Arm a wake at successor arrival + budget on those reader paths, re-armed
  when the successor changes.
- The swept benchmark measures cost and picks between evaluating the wall
  term on a timer and evaluating it lazily on access, for retention only. It
  no longer decides the semantics.
- Ranked in the clock chain right after the untimed model, because that
  model ships the failover regression below until this lands.
- Docs update inline: `track::Info::max_age` and `Subscription` docs,
  js/net's `Info.maxAge` doc, `doc/concept`, the relay config docs, the
  Expiration section and Max Age field definitions in
  `drafts/draft-lcurley-moq-lite.md` (which today exclude wall-clock
  reclamation from the rule), and the untimed text in
  `drafts/draft-lcurley-moq-timestamp.md` that the untimed model adds
  ("never media-stale, starts at the latest group"). No new guide.

Facts, and work carried over:

- Today Rust's `max_age` is media time and is applied only when the track
  writes: a group ages out when a later one starts (`is_stale` and the expiry
  scans in `rs/moq-net/src/model/track.rs`). A track that stops writing keeps
  groups past `max_age` until the pool's idle expiry (`Pool::gc`, driven by
  the origin driver without a write) or byte pressure reclaims them.
  `max_age_does_not_drive_wall_eviction` pins that behaviour and changes here.
- JS matches Rust's media-time rule since
  [#4659](https://github.com/moq-dev/moq/pull/4659): `#reach` and the drift
  check in `js/net/src/track.ts` mirror `track.rs`, and a private
  `CACHE_WINDOW_MS` idle window drives `#prune`, as Rust's pool does. The
  remaining JS work is the wall term, and rewriting `Info.maxAge`'s doc,
  which says a congestion stall cannot age content out.
  [Generated @moq/net](/quest/m1/rs2ts/README.md) replaces the JS model later;
  the maintainer chose to fix it by hand first.
- Bench in `rs/moq-net/benches/track.rs`, swept over tracks (1 to 10k) and
  cached groups per track (1 to 1k): write-path cost, timer cost per driver
  pass, and retained memory for idle tracks. A cost that grows with the table
  should show as a slope. `js/net/bench/track.ts` covers the JS side.
- Failover (from the retired untimed-failover quest, found in #4822):
  `Recover::poll` (`rs/moq-net/src/model/resume.rs`) gives up a group the
  serving route can't continue once `poll_stale` reports drift past the
  reader's budget. `drifted` needs a timed live edge and a timed successor,
  so on an untimed track nothing convicts the group and its reader waits
  until the track ends; the replaced copy's lease stays alive too. The wall
  term convicts it. Settle what "nothing pending" means against the recovery
  fetch (`a_pending_recovery_fetch_does_not_disable_expiry`), so a fetch
  about to fill the group isn't cut short. A FETCH reader has no budget and
  stays as it is.
- Start resolution: the untimed model starts an untimed track at the latest
  group (`TrackState::untimed_start`). Replace that special case with the
  normal rule, replaying the cached groups that aren't stale. Until then a
  reader rejoining an untimed track starts at a stale cached group and reads
  on from there, so `rejoin_during_the_cancel_skips_the_cache`
  (`rs/moq-net/tests/rejoin.rs`) checks only the versions whose tracks arrive
  timed. Make it check every version again.

Tests, with mocked time:

- A stalled track keeps its newest group and drops superseded ones by the
  wall rule.
- A track whose PTS stops advancing still evicts by wall clock.
- A startup burst evicts by media time.
- A model test in `resume.rs` fails over an untimed track with an abandoned
  group, against a cold route and against one whose copy already caches
  untimed groups past the resumed one, so a cursor that skips ahead can't
  hide the stall. Nothing writes after the successor arrives, so only the
  deadline wake can release the reader. It fails without the fix.
- An untimed subscriber starts at the oldest group that isn't stale.

Public API: no signature change; `max_age` behaviour changes. Wire: no
encoding change; Max Age semantics in the lite draft change.

## Related

- [Subscriber max-delay](https://github.com/moq-dev/moq/pull/4917) - renames the subscriber budget to `max_delay`; the rule applies under either name
- [JS track handover](/quest/m1/js-group-handover.md) - mirrors the failover rule in JS
- [Cache expiry growth](/quest/m1/cache-expiry-growth.md) - relay memory past the expiry window, in the same cache
- [Cache shard](/quest/m2/cache-shard.md) - the pool's shared counters under many workers
- [Generated @moq/net](/quest/m1/rs2ts/README.md) - retires js/net's hand-written model
