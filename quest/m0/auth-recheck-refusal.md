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

- `Client::post` (`rs/moq-auth/src/client.rs`) refuses on the terminal
  statuses, 401 and 403, and on a 2xx whose grant fails validation
  (`UselessGrant`, `GrantExpired`, `UnboundedRevalidate`, `ZeroRevalidate`).
  Every other status stays the outage path: 408 and 429 are an intermediary
  asking for time, 404 and 400 are a misconfiguration, and 5xx, timeouts,
  and transport errors are what the path already covers. A
  `lease::Reason::Invalid` names the decider's bug in the `end` event so
  the operator sees which side was wrong.
- A few seconds of clock skew apply to both `Grant::validate` and the lease
  deadline `Client::drive` schedules from `expires`; today a grant whose
  `expires` sits a second in the past because the auth server's clock runs
  behind the relay's is refused as "unavailable" (502), and relaxing only
  the validation would admit it and revoke it as expired on the next tick.
  Test that such a lease stays live for the skew window.
- Regression in `rs/moq-relay/tests/auth_lifetime.rs`: a `revalidate` answered
  with `Grant::default()` ends the session with `Reason::Refused`, and one
  answered with 401 does the same. The 403 and 503 cases already exist.

Public API: `lease::Reason` gains a variant (it is `#[non_exhaustive]`).
Wire: the auth `end` event's `reason` gains the value `invalid`; add it to
the `@moq/auth` schema, `doc/bin/relay/auth.md`, and the shared interop
vector in the same PR.

## Related

- [Lease clock](/quest/m2/auth-embedder.md) - the re-check driver an embedder reuses once the lease owns it
