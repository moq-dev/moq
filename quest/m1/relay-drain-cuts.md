# [S] The relay reports native sessions it force-closes at the drain deadline

## Goal

An embedder running moq-relay's graceful shutdown (GOAWAY, the drain deadline,
then teardown) can tell, through the public API, which native sessions the
deadline cut while their transport was still open, apart from sessions whose
transport had already closed and were only finishing their end report. A drain
alert can then page on real forced cuts and stay quiet for sessions that were
just cleaning up. Native sessions are the ones `supervise` runs: those the
relay's own QUIC, WebTransport, and WebSocket listeners accept, and those an
embedder hands it. A connection whose SETUP reply was still pending at the
deadline never reaches `supervise`; it stays a refused session, which its
stats already count, and is not a forced cut.

## Plan

Decided 2026-11-04 (maintainer): m1, so the public drain API changes before
the cut.

Today the relay already counts the right thing but keeps it private.
`shutdown::Tally` (`rs/moq-relay/src/shutdown.rs`) is `pub(crate)`: `live`
counts sessions still open or still reporting their end, and `forced` counts
sessions whose `drain_session` was still waiting at the deadline (the
`Draining` guard's drop). `Observer::drained` only logs `forced`, and
`session::View`, the registry entry `GET /sessions` lists, stays registered
until `supervise` has reported the end, with nothing saying the session has
closed. An embedder watching either sees a session finishing its end report
the same as one whose transport is still open, so it can only log forced
closes rather than alert on them.

- **The live state.** Split `Tally::live` into `open` (served, transport not
  yet closed) and `ending` (closed, still reporting its end). `supervise`
  moves its `Serving` from open to ending when the session closes, before it
  awaits the end report, and the `Serving` records the instant it did. Expose
  the split as a public `shutdown::Tally { open, ending }`, read through
  `Observer` as a snapshot and as a way to wait on changes (over the existing
  `watch` channel), so a drain alert can sample it during the drain. The
  bookkeeping counts (`draining`, `forced`) stay crate-private.
- **The drain's outcome.** `Observer::drained` returns a public
  `shutdown::Drained { forced, ending, elapsed }` instead of `()`, a separate
  type from the live tally:
  - `forced`: sessions whose `drain_session` was still waiting at the
    deadline, decided by the `Draining` drop as today. A session `supervise`
    observed closed strictly before the deadline is never counted as forced,
    however long its end report takes; a peer leaving in the instant of the
    deadline still counts, the sliver the `Draining` comment already accepts.
  - `ending`: sessions that moved to ending strictly before the deadline and
    were still reporting their end at it. Decided per session when its
    `Serving` drops (closed before the deadline, dropped at or after it), the
    same way `Draining` decides `forced`, rather than from a snapshot taken
    when `expired()` wins: at the deadline a forced session's task may already
    have moved to ending, so a snapshot would race it.
  - `elapsed`: from the trigger to the drain's end. The first `drained()` to
    see the drain end records its end time in the shared state, so every call,
    first or later, returns the same summary.

  Keep the existing log line, built from the same summary.
- **Per session.** The registry `Entry` (`rs/moq-relay/src/session.rs`) gains
  a shared close-time cell that its `Registration` sets; `supervise` sets it
  through the registration at the same point it moves its `Serving` to ending.
  `Registry::list` copies it into a new `session::View` field, `closed:
  Option<Wall>` (unix seconds in the `GET /sessions` JSON, null while open),
  so the sessions list and an embedder tracking sessions by id can tell an
  open session from one finishing its end report, and when it closed.
- Test on simulated time with real sessions through `supervise`, extending
  `the_drain_waits_for_a_closed_session_to_report_its_end`
  (`rs/moq-relay/tests/integration/auth_lifetime.rs`), which already holds an
  end report with `Script::hold_ends`: one session closes before the deadline
  and holds its end report past it, another stays open until the deadline
  closes it. `Drained` counts one forced and one ending, the live tally showed
  the first as ending before the deadline, and the registry view had its
  `closed` set. Assert the live tally before the deadline fires, not at it,
  since the forced session moves to ending as soon as the deadline closes it.
- Extend the existing unit tests in `shutdown.rs` rather than adding new ones:
  `the_deadline_waits_only_for_sessions_served_before_it` (a session served
  after the deadline stays uncounted in `open`, `ending`, and `Drained`) and
  `a_session_open_at_the_deadline_is_forced` (the instant-of-the-deadline
  case).
- Note the downstream consumer generically in the docs and
  `rs/moq-relay/MIGRATE.md`: an embedder's drain alert pages on
  `Drained::forced`, not on every session still counted at the deadline,
  since those include sessions already closed and only finishing their end
  report (`Drained::ending`).

Public API: `Observer::drained` returns `shutdown::Drained`, a public
`shutdown::Tally { open, ending }` with its snapshot and change stream on
`Observer`, and the new `session::View::closed` field (also in the
`GET /sessions` JSON). Wire: none.
