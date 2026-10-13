# [S] The handshake deadline covers admission

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

Tests on controlled time for both listeners: an admission that never answers
closes at the deadline, one answered just inside it is admitted, and `"0"`
waits. Update `doc/bin/relay/config.md`, which says the deadline covers
"transport through MoQ SETUP", and note the behavior change for slow auth
servers in the relay changelog.

Public API: none. Config: `listen.timeout` now includes admission. Wire: none.
