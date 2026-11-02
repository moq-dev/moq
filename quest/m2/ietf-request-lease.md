# [L] moq-transport requests end through one lease

## Goal

Every moq-transport request on both sides of `rs/moq-net/src/ietf/` owns
its stream and every way it can end (peer withdrawal, gate denial, validity,
track close, session close, grant end) behind one lease polled first on each
wake. Request bodies become plain awaits, a new await cannot miss a
terminal, and `publisher.rs`/`subscriber.rs` split by mechanism into modules
under about 2k production lines each.

## Plan

From the 2026-10-24 pre-cut audit at 151a529670. Internal only.

**Why.** The dominant source of repeated IETF fixes is a request missing
one terminal at one await:
[#2993](https://github.com/moq-dev/moq/pull/2993),
[#3004](https://github.com/moq-dev/moq/pull/3004),
[#4658](https://github.com/moq-dev/moq/pull/4658),
[#4711](https://github.com/moq-dev/moq/pull/4711),
[#4918](https://github.com/moq-dev/moq/pull/4918),
[#5654](https://github.com/moq-dev/moq/pull/5654),
[#5998](https://github.com/moq-dev/moq/pull/5998),
[#6061](https://github.com/moq-dev/moq/pull/6061),
[#6237](https://github.com/moq-dev/moq/pull/6237),
[#6409](https://github.com/moq-dev/moq/pull/6409),
[#6433](https://github.com/moq-dev/moq/pull/6433),
[#6643](https://github.com/moq-dev/moq/pull/6643), plus the
[cancel fix](https://github.com/moq-dev/moq/pull/7016) and the
[withdraw fix](https://github.com/moq-dev/moq/pull/6964). Each request
is a long `async fn` whose every `kio::wait` closure re-lists the right
subset of about 46 terminal checks; the two files hold 227 `pin!` sites
against lite's 31, a pattern `rs/AGENTS.md` calls a smell.

**Sketch.** A per-request `Lease` owns the stream and every terminal behind
one `poll_ended(waiter) -> Poll<Ending>`. `request::run(lease, body)` polls
the lease first on every wake, drops the body future on any `Ending` (RAII
releases its state), then writes what that ending owes: REQUEST_ERROR,
PUBLISH_DONE, or a cancel/reset. `crate::auth::until`
(`ietf/publisher.rs` about line 1485) already does this for the grant;
widen it. `until_withdrawn` and the per-step closures go away.

**Split.** Extend the existing `subscriber/{namespaces,tracks}.rs` pattern:
`publisher/{subscribe,fetch,namespaces,serve}.rs` and
`subscriber/{alias,announce,subscribe,ingest,fetch}.rs`. Each file today is
about 6k production lines and 12k test lines, with production code after
the test modules. Tests move with their module; the `ScriptedSession`
session tests go to `tests/integration/` per `rs/AGENTS.md`. The split cuts
merge conflicts; the lease is the lever on fix count.

Consider the same shape for lite's request machines, where the audit found
"abandoned, revoked, or retired before the answer" re-implemented in
`TrackInfoFetch`, `Establish`, `FetchServeRun`, and `TracksRecv`.

Public API: none. Wire: none.

## Required

- [moq-transport 18 and 22 only](/quest/m1/ietf-drop/README.md) - deletes about 2,950 production lines and the adapter first
