# [M] moq-transport requests name their auth

## Goal

The `draft-lcurley-moq-auth` extension mirrors moq-lite-07's named requests
for moq-transport, additively: AUTH's Request ID becomes the auth's ID, and a
new request parameter names the auth that admits a request. A named request is
admitted only by that auth's grant and ends when the auth ends or narrows away
from it, matching lite.

## Plan

Split from Requests name their auth, which landed the lite-07 wire, the Rust
`auth::Authority` that holds a request to its named token, and FIN-linger
withdrawal for gapless renewal. The decisions are recorded in the
[auth README](/quest/m1/auth/README.md).

- ✅ Decided 2026-10-15 (maintainer): one kind of auth. Each request names at
  most one; moq-transport's per-request token is only an encoding of that
  model, never a separate API.
- Settled in the #5148 design: at most one token per moq-transport request.
  Distinct tokens are refused NOT_SUPPORTED, and a repeated identical token is
  refused MALFORMED_AUTH_TOKEN. Test both.
- The alias types map as: USE_VALUE registers a request-lifetime auth and
  names it; REGISTER registers a session-lifetime auth and names it for the
  carrying request, so that request ends when the auth ends or narrows away
  from it; USE_ALIAS on a later request names it; DELETE withdraws one.
- Reuse `auth::Authority` and the peer tracking in `rs/moq-net/src/auth.rs`
  rather than a moq-transport copy. Withdrawal on moq-transport is still a
  reset; decide whether the extension gains a FIN-linger like lite's.
- Update `drafts/draft-lcurley-moq-auth.md`, Rust and `js/net`, the
  `named` bench in `rs/moq-net/benches/auth.rs` for the IETF path, and
  `doc/concept/standard.md`, which describes only moq-transport request
  tokens. Run `just test interop --all`.
- The JS acceptor decodes a lite request's `Auth` but holds every request to
  its origin scopes. Enforce named grants there too, sharing the logic with
  the moq-transport path.

Public API: none; naming is automatic.
Wire: an additive request parameter in `draft-lcurley-moq-auth`.
