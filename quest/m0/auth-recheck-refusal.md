# [S] A re-check that no longer grants ends the session

## Goal

A relay session ends when its auth server stops vouching for it, not when
its grant happens to expire. Today `moq_auth::Client` treats only a 403 as a
refusal; a 2xx whose body fails `Grant::validate` (an empty grant, which
`doc/bin/relay/auth.md` calls a refusal), a 401, a 404, or a 400 on
`revalidate` is logged as an outage and the session keeps running until
`expires` with jittered retries. A server that revokes by answering `{}` does
not revoke anything.

## Plan

- `Client::post` (`rs/moq-auth/src/client.rs`) refuses on every 4xx and on
  a grant that fails validation (`UselessGrant`, `GrantExpired`,
  `UnboundedRevalidate`, `ZeroRevalidate`); 5xx, timeouts, and transport
  errors stay the outage path. A `lease::Reason::Invalid` names the decider's
  bug in the `end` event so the operator sees which side was wrong.
- `Grant::validate` allows a few seconds of skew on `expires <= now`; a
  short-lived grant from a server whose clock runs ahead is refused as
  "unavailable" (502) today instead of admitted.
- Regression in `rs/moq-relay/tests/auth_lifetime.rs`: a `revalidate` answered
  with `Grant::default()` ends the session with `Reason::Refused`, and one
  answered with 401 does the same. The 403 and 503 cases already exist.

Public API: `lease::Reason` gains a variant (it is `#[non_exhaustive]`).
Wire: none.

## Related

- [Lease clock](/quest/m2/auth-embedder.md) - the re-check driver an embedder reuses once the lease owns it
