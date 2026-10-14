# [M] The handshake deadline covers admission

## Goal

`listen.timeout` bounds a connection from its first packet through auth
admission, on both the tokio and io_uring listeners. A peer that finishes MoQ
SETUP can no longer hold a connection open past the deadline while auth is
pending, and admission has no separate hardcoded bound.

## Plan

Decided 2026-10-13 (from #5244's review, where Codex asked to keep enforcing
the deadline while authorization runs):

- ✅ One deadline through admission. The handshake deadline that already
  bounds transport, SETUP and SETUP OK also races `Auth::admit` in
  `rs/moq-relay/src/connection.rs` and `rs/moq-relay/src/uring.rs`. On
  expiry the session closes with the MoQ timeout code, as for the other
  phases.
- `ADMIT_TIMEOUT` (10 s in `rs/moq-relay/src/auth.rs`) stops bounding
  admission; a fixed cap that only throws is not a contract. `listen.timeout
  = "0"` then waits forever through admission too, as it documents.
  Grant renewal in `auth/refresh.rs` keeps its own bound for now; it runs
  after the handshake, so `listen.timeout` does not apply.
- Not chosen: keep `ADMIT_TIMEOUT` as an inner cap; a separate
  `auth.timeout` config with `listen.timeout` still ending at SETUP.

Start after #5244 (io_uring handshake deadline) merges, since it adds the
io_uring deadline this extends.

Gaps found in this plan's review, to settle with the maintainer at start:

- Dropping the shared cap leaves the other `Auth::admit` callers unbounded:
  WebSocket admission before the upgrade (`websocket.rs`), HTTP admission
  (`admit_http` in `web.rs`), `serve_client` in `rs/moq-cli/src/main.rs`
  for `moq --listen`, and the public `Auth::admit` and `Cluster::admit` for
  embedders. Recommendation, following the decision above: the web callers
  race the web listener's timeout, which is the same `listen.timeout`;
  `moq --listen` races its own listener's `listen.timeout`; and `Auth::admit`
  and `Cluster::admit` document that they wait on the decider and the caller
  owns the deadline.
- `moq_auth::Client` sets a 10 s reqwest timeout on every request
  (`rs/moq-auth/src/client.rs`), so an HTTP auth server answering `connect`
  after 10 s is still refused. Only `connect` drops that fixed bound and
  inherits the caller's deadline. `attach` (in-band AUTH, which shares the
  `connect` path), `revalidate` and `end` run after the handshake and keep
  theirs, like grant renewal, so a stalled server cannot pile up requests.
- `moq_tokio::server::Request` keeps its handshake deadline private and
  applies it only in `ok()`, after admission, so `connection.rs` cannot race
  admission against it today. Recommendation: a public `Request` method that
  bounds a future by what remains of the deadline, keeping the deadline itself
  private, mirrored on the io_uring request if it has the same shape. Do not
  start a fresh timeout at admission; the deadline runs from the first packet.

Race inside the task that owns the admission, or cancel it, so an expired
deadline does not leave admission work running detached.

Tests on controlled time for each caller, the embedded decider and an HTTP
auth server: an admission that never answers closes at the deadline, one
answered just inside it (past 10 s for the HTTP server) is admitted, and
`"0"` waits. At the deadline the HTTP server sees its `connect` request
dropped, so expired admissions leave no in-flight requests behind. An
`attach` to a stalled server is still refused at its bound.
Update `doc/bin/relay/config.md`, which says the deadline covers
"transport through MoQ SETUP", and `doc/bin/relay/auth.md`, which promises
that an embedded admission left unanswered for ten seconds is refused; the
embedder now owns that deadline. Note the behavior change for slow auth
servers in the `moq-relay` and `moq-auth` changelogs.

Public API: a new `moq_tokio::server::Request` method to bound admission by
the handshake deadline. `Auth::admit`, `Cluster::admit` and
`moq_auth::Client::connect` keep their signatures but no longer time out on
their own. Config:
`listen.timeout` now includes admission. Wire: none.
