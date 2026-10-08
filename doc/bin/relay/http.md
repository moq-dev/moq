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

`moq announced` and `moq fetch` answer the same questions over MoQ; see
[Inspect a relay](/bin/inspect).

GET routes on the public router, including ones an embedder merges in, get a
wildcard CORS origin unless the route sets its own. Tokens sent over plain HTTP
are visible on the wire, so use HTTPS in production.

## Internal

```toml
[internal]
listen = "127.0.0.1:9101"
```

### GET /metrics

Prometheus text: bytes, frames, groups, subscriptions, viewers, and sessions
split by `tier` and `role`, plus accept-loop counters per TCP listener. Traffic
counters accumulate for the node's lifetime and leave out the stats feed's own
traffic. Host CPU and memory belong to a node exporter.

A few worth alerting on:

- `moq_relay_accept_failures_total{class="exhausted"}`: the process ran out of
  a resource `accept` needs.
- `moq_relay_sessions_refused_total`, by `reason`. A rise in `unavailable`
  points at the auth server; a rise in `refused` at the credentials clients
  present. Each attempt counts, so a client that retries is counted again.
- `moq_relay_stale_bytes_total` and friends: content dropped for drifting past
  a subscriber's latency budget.
- `moq_relay_draining_sessions`: sessions sent a GOAWAY during a
  [shutdown drain](/bin/relay/config#shutdown) that have not left yet.

With `--runtime-io-uring`, each QUIC worker also reports `moq_relay_uring_*`
counters under a `worker` label. A dead worker shows stuck zeros rather than
disappearing.

### GET /sessions

Live sessions on this node. Query parameters filter on the fields the auth
server already saw (`id`, `path` as a pattern, `remote` as an IP or CIDR,
`transport`, `tls.name`, ...); every given field must match, and an empty
filter is everyone. Each entry is the auth request plus start time, without
`query` or `token`.

```bash
curl 'http://127.0.0.1:9101/sessions?path=demo/**'
```

### POST /sessions/revalidate

The same filter, re-checked now: the relay asks the auth server again for each
match and applies its answer. See [push](/bin/relay/auth#push). One node, no
cluster fan-out.

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
