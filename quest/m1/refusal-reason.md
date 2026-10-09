# [M] Typed refusal reason

## Goal

A refusal tells the relay why and for whom. `moq auth serve` answers a 403
with a JSON body carrying the reason (including `expired`) and, optionally,
the root and tier it resolved, and `moq_auth::Client` parses it into a typed
error. A legacy plain-text 403 (or 401) still reads as
`refused`. An embedder's decider can refuse with the same root and tier. The
relay's session outcomes then count `expired` refusals and attribute a
refusal to that root and tier.

## Plan

Decided 2026-10-08: moved out of the auth line to m1, since it extends
session outcomes and the HTTP auth contract, not in-band AUTH.

Decided 2026-10-08 (split from [Session outcomes](/quest/m1/session-outcomes.md)):

- **Why.** `TokenExpired` is raised only in moq-auth's key check
  (`rs/moq-auth/src/key.rs`), on the auth server. `serve.rs` sends every
  refusal as a plain-text 403, `client.rs` maps 401/403 to `Error::Refused`
  and discards the body, and the relay verifies no token itself. So neither
  the reason nor the resolved root reaches the relay today.
- **Contract.** A refusal body names a reason (`refused`, `forbidden`, or
  `expired`; the relay's other reasons are its own), an optional root, and an
  optional tier. Unknown reasons read as `refused`, so the set can grow. The
  root and tier are the auth server's claim, trusted like a grant's. A
  third-party auth server is a consumer of this contract: document it in
  `doc/bin/relay/auth.md` and `doc/lib/rs/moq-auth.md`.
- **Serve.** `verify` folds `TokenExpired` into `Refusal::InvalidToken`'s
  string today (`rs/moq-auth/src/serve.rs`); add an expired refusal and map
  serve's `Refusal` variants onto the three reasons.
- **Bounded attribution.** A body names a root only when a verified token
  named it, never the dialed path (`NoAccess { path }` is attacker-chosen), so
  a client cannot mint a stats entry per random path.
- **Embedder.** An embedder deciding admission (moq.pro's edge) refuses
  through `Admission::refuse` (`rs/moq-relay/src/auth.rs`), whose
  `auth::Error` has no `expired` and carries no root or tier. Give the HTTP
  path and `Admission::refuse` one refusal shape (reason, optional root and
  tier), under the same bound, and update `From<moq_auth::Error>`, which sends
  anything but `Refused` to `Unavailable` today.
- **Relay.** Map the typed error to a `Refusal` reason (adding `expired`) in
  `/metrics`, and count it in the root's `Presence` on that tier, or the
  default tier when the body names none. A refusal without a root stays
  unattributed.
- Tests: the serve/client round trip for each reason with and without root
  and tier; a legacy plain-text 403 reads as `refused`; an expired token, an
  auth server refusal on a named tier, an embedder `expired` refusal, and an
  embedder refusal on a named tier each count under the right root, tier, and
  reason; a refusal for an
  unverified token stays unattributed.

Public API: moq-auth's refusal error and the HTTP refusal body. Wire: the
auth HTTP contract only, not MoQ.

## Required

- [Session outcomes](/quest/m1/session-outcomes.md) - the refusal counters
  this attributes and extends with `expired`

## Related

- [Expired token error](/quest/m1/auth/expired-error.md) - the MoQ wire
  half: a session whose token expired reports `Error::Expired`
