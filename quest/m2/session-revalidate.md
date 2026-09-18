# [M] Push a re-check to live sessions

## Goal

An operator or auth server makes a relay re-check sessions now instead of
waiting for the grant's `revalidate` cadence, so a kick, a gate, or a retier
lands in one round trip and the cadence can go back to minutes. The relay's
internal listener gains `POST /sessions/revalidate` and `GET /sessions`, both
taking the same filter: any subset of the fields the auth server already saw
in the session's request (`id`, `path` as a pattern, `remote` as an address or
CIDR, `transport`, `tls.name`, ...), every given field must match, and an
empty filter is every session on this node. A push does not decide anything:
the relay re-POSTs `revalidate` and the server's reply kicks, narrows,
retiers, or keeps the session exactly as a scheduled re-check would. An
embedder holding the `lease::Producer` sees the same nudge in process, and
`moq auth revalidate` posts it from the command line. Additive on the
lease and the relay dev already has, so it starts on `main` once dev merges.

Boundaries: one node, no cluster fan-out; the server knows each session's
`node` from `connect` and calls that relay. The contract's JSON is untouched
and `@moq/auth` does not change. Closing a session directly, with no server
in the loop, is not a route: a re-check is not an authority, which is what
lets it live on the unauthenticated internal plane.

## Plan

- **The nudge crosses the lease.** `moq_auth::lease::Consumer::revalidate()`
  asks the producer side to re-check now; `Producer::poll_revalidate(waiter)`
  and `Producer::revalidate_requested()` resolve once however many asks
  arrived since the last observation, so a burst coalesces. A
  `Consumer::fixed` lease has nobody to ask and the call is a no-op. Both
  are additive on the lease's shared state beside `epoch`.
- **The client honors it.** `Client`'s driver selects on the request: idle
  or in backoff, it POSTs `revalidate` at once and clears the scheduled
  time; with a re-check in flight it remembers one more and POSTs again
  when the reply lands, since a ban set after the in-flight request left
  would otherwise be missed until the next cadence. A refusal, a fresh
  grant, or a failure is applied as today. The in-process example in
  `doc/bin/relay/auth.md` gains the `revalidate_requested()` arm so an
  embedder's decider re-runs its decision on a nudge.
- **The relay keeps a registry.** `moq_relay::session::Registry`, cloned
  into every `Connection`, holds each admitted session's `moq_auth::Request`
  as the server saw it, its start time, and a handle that wakes `supervise`,
  which then calls `revalidate()` on the lease it owns. A session registers
  after `admit` and leaves on close, io_uring and tokio sessions alike, and
  so does the built-in WebSocket path: `serve_ws` in `websocket.rs` admits
  and runs `handle_socket` without a `Connection`, so it takes the registry
  through `WebState` and selects on the same wake. LAN peers and one-shot
  `http` requests are not sessions and stay out.
  `Relay::sessions()` exposes the registry so an embedder's own listeners
  register too.
- **The filter.** `session::Filter` is the partial request: `id` and every
  scalar field are exact matches, `path` is a `moq_pattern::Pattern` matched
  against the dialed path, `remote` is an IP or CIDR matched with the port
  dropped and IPv4-mapped IPv6 folded, and `tls.name`, `tls.fingerprint`,
  and `tls.issuer` reach the certificate facts. `query` is not a filter
  field: it may carry the credential, and an equality match on it would be
  an oracle for one. An unknown field is a 400 naming it, never ignored. The filter rides as query parameters on both
  routes, so a curl needs no body and one encoding serves list and push.
- **The routes.** On the internal listener beside `/metrics`:
  `GET /sessions` returns `{"sessions": [...]}` with each match's request
  and start time, the dry run for a selector and an ops inventory. The
  entry is a dedicated view, never the stored request: `query` is omitted,
  since a `jwt` in it would let anyone on the plane replay every live
  token;
  `POST /sessions/revalidate` nudges every match and returns 202 with their
  ids, the re-checks running in the background and their outcome arriving
  as `end` events. No match is 200 with an empty list, not a 404. The
  module doc's line that a control endpoint would need its own auth is
  rewritten: this one can only cause re-checks, so a caller on the trusted
  plane gains nothing a scheduled cadence would not do.
- **The CLI.** `moq auth sessions` and `moq auth revalidate` take
  `--internal-url` and one flag per filter field (`--id`, `--path`,
  `--remote`, `--transport`, `--tls-name`, ...) and print the reply as
  JSON. `doc/bin/cli.md` shows a kick by id and a path-wide push.
- **Docs.** `doc/bin/relay/auth.md` gains a "Push" section after
  "Revalidate and outage" with the curl form, the rule that a push is a
  re-check, and the note that an empty filter re-POSTs once per session,
  so a node under load is nudged through a selector, `doc/bin/relay/config.md`'s `[internal]` lists the two routes,
  and the CHANGELOGs of moq-auth, moq-relay, and moq-cli record the
  additions.
- **Tests.** moq-auth: a nudge while idle POSTs at once, during backoff
  POSTs at once, during an in-flight re-check POSTs exactly once more when
  the reply lands, on a fixed lease does nothing, and N nudges wake an
  embedded producer once. moq-relay, against an in-process server whose
  reply flips: a push by id closes a refused session with `Unauthorized`
  well inside the cadence and its `end` says `refused`; a path pattern
  and a CIDR each match the right subset of three sessions and leave the
  rest untouched; a WebSocket session is listed and closes on a push like
  a QUIC one; an empty filter reaches all; a retier arrives on the
  next reply; `GET /sessions` lists what the push would match without
  its `query`; an unknown field and a `query` filter are refused. The CLI
  test drives `moq auth revalidate` against the same fixture.

Public API: additive, `lease::Consumer::revalidate`,
`lease::Producer::{poll_revalidate, revalidate_requested}`,
`moq_relay::session::{Registry, Filter}`, `Relay::sessions()`, two internal
routes, and two `moq auth` subcommands. Wire: none.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - builds on the moq-auth lease and
  the relay admission path that only dev has

## Related

- [Stats retier](/quest/m2/stats-retier.md) - what a pushed re-check's new
  `tier` should do to the live session
- [Origin narrowing](/quest/m2/origin-narrowing.md) - what a pushed
  re-check's narrower grant should do instead of closing
- [One admission path](/quest/m1/auth-one-path.md) - the embedded producers
  that honor the same nudge
- [Relay tokens](/quest/m2/auth/relay-refresh.md) - a session holding one
  lease per in-band token nudges each of them
