# [M] A busy task yields within a budget

## Goal

No kio task can hold its thread with an unbounded loop of async kio
readiness polls while work keeps arriving. Synchronous `try_*` and peek
loops, and code outside a `kio::coop::budget`, stay unbounded, as today. A serve loop that always has another group ready yields after a
fixed budget of progress and resumes on its next poll, so the QUIC driver,
timers, and the session's other tasks run in between, on every runtime
(tokio multi-thread and current-thread, wasm, and the io_uring worker).

Found while landing #4225 (2026-10-08): the go interop publisher writes
2.5 ms Opus frames, one group each (about 400 groups/s). When a relay
subscribes, `RequestServe::poll_serve` (`rs/moq-net/src/lite/publisher.rs`)
keeps returning `Continue` and one poll ran over 4,096 iterations. moq-ffi's
current-thread runtime then starves noq's connection driver until the relay
times the publisher out after 10 s. Hosted Interop's `go -> *` lanes fail on
#4225 and stall for about 10 s on `main`. The gate is the mocked serve test
below, not those lanes: the m1 quests each hide the stall on their own, and
the #4225 root-cause comment saw a 64-iteration yield still time the
publisher out, so a yield alone may not be the whole fix. Re-check the lanes
and #4225 when this lands.

Not here: moq-net spawning tasks (it stays poll-only by design, #2302,
#2736, #3825), the FFI runtime flavor, or the audio grouping default; those
are separate quests below.

## Plan

Decided 2026-10-08 in a `/quest-plan` interview (paper trail in the PR that
added this quest):

- The bound is generic, modeled on tokio's cooperative budget, and lives in
  kio, which takes no runtime dependency. Tokio's budget cannot see this loop,
  because the work goes through kio primitives, never a tokio resource.
  Rejected: per-loop budgets in the two serve loops (not generic), and
  bridging kio to `tokio::task::coop` (leaves wasm and io_uring unbounded).
- Mechanics:
  - a per-thread budget in kio. An async kio readiness poll that makes
    progress spends one unit, and refunds it if the operation ends `Pending`
    anyway;
  - an exhausted budget wakes the current task
    (`waiter.waker().wake_by_ref()`) and returns `Pending`;
  - synchronous `try_*` and peek APIs, and any poll with
    `kio::Waiter::noop()`, spend nothing, since their callers read "nothing
    now" as empty, not as "come back". `Waiter` carries an explicit noop
    flag for this, since comparing wakers is not reliable.
- A budget `Pending` must only postpone, never change a decision or lose
  state. Spend only at state-safe boundaries, and audit every caller that
  reads `Pending` as a value or commits state before a later poll. Known
  sites: `TrackRun::start` reads Largest for SUBSCRIBE_START with
  `Pending => None`; `poll_recv_next` probes `poll_finished` after
  `poll_recv_group` ends, and a `Pending` there skips SUBSCRIBE_END;
  `GroupServe` reads `poll_expired` as a bool; and `Cursor::poll_recv_group`
  advances `index` or takes a parked group before `poll_stale`, so a
  `Pending` there drops the group. Tests drive each site to exhaustion and
  check every eligible group is delivered exactly once, fresh and parked.
- The budget is per kio task: `Tasks::poll` refills it for each child it
  polls, and `moq_net::time::run` refills it once per `kio::wait` poll,
  outside its loop, so an always-ready timer can't refill per iteration. A child that runs out lands in the next pass by
  `Tasks`' one-snapshot rule (`a_self_waking_task_yields_to_the_owner`), and
  the owner returns to its runtime after the pass. Work per turn scales with
  the children that were ready, so a busy fan-out session is not throttled.
  If [Run to quiescence](/quest/m1/perf/uring-quiescence.md) lets a turn run
  several passes, each pass refills every child it polls, so a turn is
  bounded by passes times ready children times the budget, and the owner
  returns only after the last pass. Owners that poll their `Tasks` more than
  once per poll (`Publisher::poll`, `SubscribeServe::poll_step` inside
  `RequestServe::poll_serve`'s loop) compound the same way: a nested serve
  can spend about the budget squared in one runtime poll. That is bounded,
  and the bench decides whether it matters.
  Rejected: one budget shared by the whole runtime task (tokio's model),
  which caps a session with many subscriptions at one budget per turn.
- Refill mirrors tokio: `kio::coop::budget(f)` sets a fresh budget and
  restores the previous value on exit, so nesting refills. Public, documented
  as called only where a driver hands a task its turn. Code outside any
  `budget` call is unconstrained, as today. The moq-uring worker's pass calls
  it too if it polls anything outside `Tasks`.
- The size starts at 128 (tokio's) and is a constant, not configurable. A
  bench picks it: sweep unbounded, 32, 128, and 512 against `main` and keep
  the smallest whose throughput matches unbounded.
- Tests, with mocked time: a producer appends 10,000 groups during a serve
  with many groups in flight, the serve poll returns `Pending` while a
  sibling task and the transport make progress (fails on `main`), and the
  units spent per runtime poll are counted against the bound; kio unit tests
  for spend, refund, exhaustion, and nested refill.
- Bench: extend `rs/moq-net/benches/session.rs` swept over groups/s and
  subscriptions per session, and kio's `channel` and `tasks` benches for the per-operation cost
  of the thread-local.

Public API: `kio::coop::budget` (new). Wire: none.

## Related

- [FFI runtime](/quest/m1/ffi-runtime.md) - FFI apps run on more than one thread
- [Audio group duration](/quest/m1/audio-group-duration.md) - small audio frames stop minting a group each
- [FFI publisher stall](/quest/m0/ffi-publisher-stall.md) - verifies the Go and Python interop cells once this lands, and fails a cell whose connection idles out
- [Run to quiescence](/quest/m1/perf/uring-quiescence.md) - its pass count bounds passes per turn; this budget bounds one task's loop, so they compose
