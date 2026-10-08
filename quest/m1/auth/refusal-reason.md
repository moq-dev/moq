# [M] Typed refusal reason

## Goal

An auth server's refusal tells the relay why and for whom: `moq auth serve`
answers a 403 with a JSON body carrying the reason (including `expired`) and,
optionally, the root and tier it resolved, and `moq_auth::Client` parses it
into a typed error. A legacy plain-text 403 (or 401) still reads as
`refused`. The relay's session outcomes then count `expired` refusals and
attribute a refusal to that root and tier.

## Plan

Decided 2026-10-08 (split from [Session outcomes](/quest/m1/session-outcomes.md)):

- **Why.** `TokenExpired` is raised only in moq-auth's key check
  (`rs/moq-auth/src/key.rs`), on the auth server. `serve.rs` sends every
  refusal as a plain-text 403, `client.rs` maps 401/403 to `Error::Refused`
  and discards the body, and the relay verifies no token itself. So neither
  the reason nor the resolved root reaches the relay today.
- **Contract.** A refusal body names a reason from session outcomes'
  vocabulary plus `expired`, an optional root, and an optional tier. Unknown
  reasons read as `refused`, so the vocabulary can grow. The root and tier
  are the auth server's claim, trusted like a grant's. A third-party auth
  server is a consumer of this contract: document it in
  `doc/bin/relay/auth.md` and `doc/lib/rs/moq-auth.md`.
- **Relay.** Map the typed error to a `Refusal` reason (adding `expired`) in
  `/metrics`, and count it in the root's `Presence` on that tier, or the
  default tier when the body names none. A refusal without a root stays
  unattributed.
- Tests: the serve/client round trip for each reason with and without root
  and tier; a legacy plain-text 403 reads as `refused`; an expired token and
  an attributed refusal on a named tier each count under the right root,
  tier, and reason.

Public API: moq-auth's refusal error and the HTTP refusal body. Wire: the
auth HTTP contract only, not MoQ.

## Required

- [Session outcomes](/quest/m1/session-outcomes.md) - the refusal counters
  this attributes and extends with `expired`

## Related

- [Expired token error](/quest/m1/auth/expired-error.md) - the MoQ wire
  half: a session whose token expired reports `Error::Expired`
