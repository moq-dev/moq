# [S] The relay reports native sessions it force-closes at the drain deadline

## Goal

An embedder running moq-relay's graceful shutdown (GOAWAY, the drain deadline,
then teardown) can tell, through the public API, which native sessions the
deadline cut while they were still open, apart from sessions whose media had
already ended and were only finishing their end report. A drain alert can then
page on real forced cuts and stay quiet for sessions that were just cleaning
up. Native sessions are the ones `supervise` runs: those the relay's own
QUIC, WebTransport, and WebSocket listeners accept, and those an embedder
hands it.

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
the same as one carrying media, so it can only log forced closes rather than
alert on them.

- **The drain's outcome.** `Observer::drained` returns a public summary instead
  of `()`: how many sessions the deadline force-closed while open, how many
  had closed before the deadline but were still reporting their end when it
  passed, and how long the drain took. A session that closed before the
  deadline is never counted as forced, however long its end report takes.
  Keep the existing log line, built from the same summary.
- **The live state.** Split `Tally::live` into sessions still open and
  sessions closed but still reporting their end, and make the tally readable
  by embedders through `Observer` (a snapshot, and a way to wait on changes,
  over the existing `watch` channel), so a drain alert can sample the counts
  as the deadline passes rather than only at the end. `supervise` moves its
  `Serving` from open to ending when the session closes, before it awaits the
  end report.
- **Per session.** `session::View` says whether each registered session is
  still open or only finishing its end report (for example, when it closed),
  so the sessions list and an embedder tracking sessions by id can tell them
  apart too.
- Test on simulated time, as `a_session_open_at_the_deadline_is_forced` does,
  with real sessions through `supervise`: one session closes before the
  deadline and holds its end report past it, another stays open until the
  deadline closes it. The summary counts one forced and one ending, the live
  tally showed each in its own count at the deadline, and the registry view
  marked the first as ending.
- Cover a session served after the deadline (closed at once, uncounted, as
  today) and a peer leaving in the instant of the deadline (forced, as the
  `Draining` drop already decides).
- Note the downstream consumer generically in the docs and `MIGRATE.md`: an
  embedder's drain alert pages on the forced count, not on sessions still
  live at the deadline.

Public API: `Observer::drained`'s return type, a public tally with the split
counts, and the new `session::View` field (also in the `GET /sessions` JSON).
Wire: none.
