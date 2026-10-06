---
title: HTTP
description: The relay's HTTP endpoints for debugging, history, health, and metrics
---

# HTTP Endpoints

`[web.http]` and `[web.https]` serve public endpoints; `[internal]` serves
operational ones that must stay private.

## Public

| Endpoint | Returns |
| --- | --- |
| `GET /announced/<prefix>` | Broadcasts announced under the prefix, named relative to it. |
| `GET /fetch/<broadcast>/<track>?group=N` | One group from the cache, the latest by default, or `404` if the track has no such group. Useful for catch-up and debugging. |
| `GET /certificate.sha256` | The fingerprint of the first configured TLS certificate, for pinning a self-signed dev certificate. |
| `GET /health` | `200 ok`, unauthenticated, for load balancers. |

```bash
curl http://localhost:4443/announced/demo
curl http://localhost:4443/fetch/demo/bbb.hang/catalog.json
```

The announcement listing names each announced route by the prefix it covers;
by convention a publisher announces each broadcast's exact path, so the list
reads as broadcast names.

`moq announced` and `moq fetch` answer the same questions over MoQ; see
[Inspect a relay](/bin/inspect).

A relay configured with more than one certificate has no single fingerprint to
publish, and this endpoint answers for the first. The others are reachable over
`https://`, which selects a certificate by SNI at the handshake.

GET responses on the public router receive a wildcard CORS origin unless the
route sets its own policy. This includes routes merged by an embedding
application. GET preflights are supported; routes using other methods configure
their own CORS policy.

Tokens sent over plain HTTP are visible on the wire, so use HTTPS in
production.

## Internal

```toml
[internal]
listen = "127.0.0.1:9101"
```

### GET /metrics

Prometheus text: bytes, frames, groups, subscriptions, viewers, and sessions
split by `tier` and `role`, plus accept-loop counters per TCP listener. Alert
on `moq_relay_accept_failures_total{class="exhausted"}`, which means the
process ran out of a resource `accept` needs. Content dropped for drifting past
a subscriber's budget is counted separately as `moq_relay_stale_bytes_total`
and friends. During a [shutdown drain](/bin/relay/config#shutdown),
`moq_relay_draining_sessions` counts the sessions sent a GOAWAY that have not
left yet. Host CPU and memory belong to a node exporter.

`moq_relay_sessions_refused_total` counts the session attempts admission turned
away, by `reason`:

- `refused`: the decider said no (the auth server, the public rules, or an
  embedder).
- `unavailable`: the decider could not answer, so the client is told to retry
  (the auth server was unreachable or answered with neither a grant nor a
  refusal, a decider sent a grant the relay cannot use, or an embedder did not
  answer).
- `request`: an embedder refused the request as one it cannot decide.
- `forbidden`: the grant allows nothing the session asked for, or an embedder
  refused the session as forbidden.
- `lan`: a LAN peer's membership proof was missing or wrong, or LAN discovery
  is off.

Only sessions are counted. The HTTP routes admit through the same decider but
open no session, so like `moq_relay_sessions_opened_total` this leaves them out,
and a refusal earlier in the handshake, such as TLS, never reaches admission. A
rise in `unavailable` points at the auth server; a rise in `refused` at the
credentials clients present.

Each attempt counts once, so this is refused attempts, not refused clients, and
one connect can be refused more than once. A client that races WebSocket against
QUIC can be refused on both when its WebSocket upgrade goes out before QUIC
connects; the browser client starts WebSocket after a head start, or at once for
a URL where WebSocket won before. A client that cannot tell a refusal from a
failure retries, and each retry counts: a browser refused over WebSocket sees no
status and retries until its reconnect window closes, while a credential refusal
over QUIC closes the session as unauthorized, which stops it.

Traffic and session counters accumulate for the node's lifetime, including
broadcasts and sessions that have ended. The stats publishing prefix (normally
`.stats`) is excluded to avoid counting the feed's own traffic.

With `--runtime-io-uring`, each QUIC worker thread also reports its own
`moq_relay_uring_*` counters under a `worker` label: datagrams and syscalls
(the ratios are the GRO/GSO batching and the syscall amortization the runtime
exists for), buffer-pool backpressure (`rx_enobufs`, `rx_exhausted`,
`tx_stalls`), cross-thread wakes, and timer churn. Every worker reports from
the moment the port is bound, so a dead one shows stuck zeros rather than
disappearing. These describe the process, not the traffic, so they never appear
on the `.stats` broadcast.

### GET /sessions

Live sessions on this node. The filter is query parameters: any subset of
the fields the auth server already saw (`id`, `path` as a pattern, `remote`
as an IP or CIDR, `transport`, `tls.name`, ...). Every given field must
match; an empty filter is everyone. Each entry is the request plus start
time, with `query` omitted. An unknown field, including `query`, is 400.

```bash
curl 'http://127.0.0.1:9101/sessions?path=demo/**'
```

### POST /sessions/revalidate

The same filter, a re-check now. Matching sessions are nudged and the
response is 202 with their ids; no match is 200 with an empty list. The
relay re-POSTs `revalidate` and the auth server's reply is the verdict.
One node, no cluster fan-out.

```bash
curl -X POST 'http://127.0.0.1:9101/sessions/revalidate?id=00ff'
```

### GET /nodes

The peers this relay dialed and holds a session with: each node's URL, without
its query, and the connections to it, with the same `conn` id the logs use.
Peers that dialed this relay are not listed, since they declare no URL.

```json
{ "nodes": [{ "node": "https://us-east.example.com/", "connections": [{ "id": 3 }] }] }
```
