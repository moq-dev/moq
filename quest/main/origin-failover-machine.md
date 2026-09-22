# [L] Origin failover as a state machine

## Goal

The origin's failover, how a front picks the source it serves from and how
each spliced track follows that choice, is a pure step function: events in,
actions out, no locks and no waiters inside. The async task around it only
feeds events and executes actions. Every transition has a test that needs no
runtime, and a bounded enumeration over event sequences checks the invariants
a wake predicate used to guard by hand: an event that changes nothing produces
no action, a source that refused a track is never dispatched for it again, a
source change produces at most one takeover per track.

Subscribers keep today's guarantees: the newest local publisher at a path wins
the moment it attaches, a remote front resumes only through routes sharing the
first hop that served it, a source refusing one track is skipped for that
track alone, and a front with nothing left to serve ends with the last
refusal's error. Behavior only: everything stays private to
`rs/moq-net/src/model/origin.rs` except the action surface of
`resume::Producer`, which may move to fit the machine. No wire or public API
change; lands on main.

## Plan

### Root problem

Every livelock and spin this file has had was a wake predicate disagreeing
with the decision it guarded: the resume memo of #3312, the `NoRoute` edge
that kept firing, the `dead` set that starved the watcher. #3882 and #3884
removed the table-wide predicates (fronts now sleep on a `Watch` at their
path node), but `serve_track` still polls `FrontState` with a predicate that
must mirror `serve_route`, and the decisions are spread across five pieces of
state per source (`refused`, `dead`, `refusal`, `spliced_edge`, `Identity`)
plus a multi-source table only local fronts use. Make the decision data, not
a closure, and the class disappears.

### Shape

- One machine per front, owning its per-track state. One driver task per
  front polls exactly the waits the machine names (the watch, the in-flight
  upstream request, the serving source's close, each spliced track's
  completion, each track's demand edge, one deadline) and hands the result
  back as an event. Tracks are entries in the machine's state with their own
  demand gate and idle linger; there is no per-track task.
- Events, roughly: a covering route changed, the upstream request resolved or
  refused, the serving source closed, a track was assigned, a track completed
  or failed (with whether it delivered anything since its splice), a reader
  arrived or the last one left, the linger deadline passed, the origin closed.
  Actions, roughly: request through a route, attach or detach a source,
  splice a track from a source, release a track's segment, abort or finish a
  track, end the front, arm a deadline. Adjust the alphabet as the tests
  demand; the point is that both are plain data.
- One source slot per front. Local publishers are already route entries
  (`local: true`) and `route_order` prefers the newest, so the table selects
  for local and remote alike and `FrontState.sources` goes away. Keep what
  `attach_source` guarantees today: a front whose sources have all closed is
  replaced by a fresh broadcast, never spliced into, and the join-or-create
  decision happens under one lock.
- Per-track verdicts stay per track (a standby that has not created every
  track must not cost the incumbent anything), but they become one enum per
  track (unspliced, waiting on info, spliced from a source, refused by these
  sources, done) instead of parallel sets. The corpse distinction goes: a
  source that is closing is never dispatched, and its watcher's detach is the
  only signal the machine waits for.
- The identity pin (first hop of the route that first served the front) stays
  the rule for which routes qualify on re-selection.

### Proof

- A transition table: for each state and event, the expected actions and next
  state, as plain unit tests.
- A bounded exhaustive enumerator over the event alphabet with a small model
  (two sources, two tracks, sequences up to a handful of steps) asserting the
  invariants above. No new dependency; a hand-rolled enumerator is enough.
- The existing tokio tests stay as the end-to-end proof and pass unmodified
  except where a dropped rule (the corpse distinction) changes what they
  assert. `benches/origin.rs` `handoff` is the before/after for the handover
  latency; it must not regress.

### Look out for

- `Watch::seen` is read under the table lock beside the decision it guards;
  a watch is never dropped while holding that lock.
- The demand gate: a release on the unused edge must not re-splice and spin;
  both directions key off the same signal today, keep that.
- `resume::Producer::takeover` errors other than `Closed` are boundary bugs;
  the machine aborts the track rather than retrying.
- Track metadata compatibility (`accept_track_info`) is a refusal, not a
  failover.
- Teardown order: `closed` is set and every watch poked under the table lock
  before requesters are rejected.

## Required

- [#3884](https://github.com/moq-dev/moq/pull/3884) merged: the front watch this builds on

## Related

- [Origin narrowing](/quest/next/origin-narrowing.md) - a live grant narrowing ends broadcasts the origin handed out; rebases onto the new shape
- [Dynamic track sequences](/quest/next/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - the takeover edge this machine drives
- [Relay memory](/quest/next/relay-memory.md) - per-front state is part of what it measures
