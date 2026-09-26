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
| `GET /announced/<prefix>` | Broadcasts announced under the prefix. |
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

`moq ls` and `moq fetch` answer the same questions over MoQ; see
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
and friends. Host CPU and memory belong to a node exporter.

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

This relay's view of the cluster: each visible node's URL, Hop ID, the route
its advertisement took, and the connections to it (with the same `conn` id the
logs use). A route is priced twice: `cost` as the cluster stands, which reads 0
through a relay already carrying the broadcast, and `cold_cost` with those
discounts removed, which is what tells two warm relays apart. It is best-effort
correlation, not authenticated identity.
