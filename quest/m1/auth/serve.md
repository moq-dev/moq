# [M] moq auth serve is the reference auth server

## Goal

`moq auth serve` answers the moq-auth contract on a loopback or unix
listener with the policy the relay used to hold, so an operator who ran
`--auth-key`, `--auth-key-dir`, or `--auth-public` moves those flags to the
server and points the relay at it. It verifies a `jwt` from the query against
its keys, grants anonymous connections what `--public-*` names, grants a peer
with a verified certificate what `--mtls-*` names and nothing without it,
labels tiers, schedules revalidation, and caps live sessions per token and per
address. The default answer to anything else is a refusal. On dev.

## Plan

- `moq auth serve --listen 127.0.0.1:4440` by default, `--listen
  unix:/run/moq-auth.sock` for a socket; a non-loopback bind needs
  `--listen-public` so nobody exposes the server by accident. One route,
  `POST /`, reading `moq_auth::Request` and writing `moq_auth::Grant`, over
  the axum the relay already depends on.
- Policy, evaluated in this order and stopping at the first that applies:
  a `jwt` in the query is verified against `--key <file>` or `--key-dir
  <dir>` (by `kid`) and its claims are the grant, its `root` required to
  equal the dialed path; a verified certificate gets `--mtls-publish` and
  `--mtls-subscribe`; an anonymous connection gets `--public-publish` and
  `--public-subscribe`. A malformed or expired token is a refusal, not a fall
  through to the anonymous rules. Each list is a pattern union and each
  defaults to empty.
- `--tier <label>` on the anonymous and mTLS grants, `--tier-websocket` and
  friends only if stats need them; the JWT carries no tier, so a token grant
  takes `--tier`. `--revalidate <duration>` sets the cadence on every grant
  (default one minute). Every grant carries an `expires`, since the contract
  refuses a cadence without one: the JWT's `exp`, the certificate's
  `notAfter`, or `--expires <duration>`, default one day, for an anonymous
  session and for a JWT signed without `exp` or a certificate without a
  bound, so the bare public-policy invocation and the default signing flow
  both produce grants the server can admit, and such a session reconnects
  daily at most.
- Session limits: `--limit-token <n>` caps live sessions presenting the same
  token and `--limit-remote <n>` caps live sessions from one remote IP, the
  `remote` socket address with its port dropped and an IPv4-mapped IPv6
  address folded to IPv4,
  counted from `connect` and `end` events by session `id`, with a
  `connect` for a known id refreshing rather than double-counting. A slot
  ages out when its session misses two revalidation cadences, so a relay
  that died without sending `end` does not pin a slot forever; the server
  cannot tell that from its own unreachability, so the next `revalidate`
  from a surviving relay re-registers the id, and the worst case is one
  session admitted over the cap for one cadence. The docs say so: the cap
  is a nuisance limit, not a security boundary, and a restart empties the
  table until the fleet's next cadence refills it. Default unlimited. A
  connect over the cap is refused with the reason in the body.
- The migration table in `doc/bin/relay/auth.md`: each deleted relay flag and
  the `moq auth serve` flag that replaces it, and a worked example running
  both on one host. `doc/bin/cli.md` documents the command.
- Tests: each policy branch, the ordering (a bad token with public rules is
  refused), the limit counters through connect, end, and aging, a unix
  listener, and the bind guard.

Public API: `moq auth serve` is new. Wire: none.

## Required

- [Package](/quest/m1/auth/package.md) - the types and the JWT it serves
