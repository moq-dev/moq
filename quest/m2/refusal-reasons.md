# [S] Refusal reasons tell an expired token apart

## Goal

`moq_relay_sessions_refused_total` (added in #4825, documented in
`doc/bin/relay/http.md`) tells an expired token from a forged or otherwise
invalid one. It also counts sessions a gateway admits through
`Cluster::admit` and `Cluster::scope`, which #4825 left uncounted.

## Plan

Found in #4825: `moq auth serve` (`rs/moq-auth/src/serve.rs`) writes its
refusal into the 403 body, but `moq_auth`'s client returns `Refused` without
reading it, and the server reports both cases as `InvalidToken` with a
message. Decided (2026-10-05): the auth server returns a structured refusal
reason and `moq_auth`'s client maps it, so `refused` can split.

Decided (2026-10-05): count gateway admissions too. #4825 counted where the
relay tells the client no, so a gateway session shows in
`moq_relay_sessions_opened_total` while its refusals don't.

Public API: `moq_auth`'s refusal error gains a reason. Wire: the auth
server's refusal body gains a structured reason; check older relays still
read it.

## Related

- [Expired token error](/quest/m1/auth/expired-error.md) - the session-level `Expired` a client sees
