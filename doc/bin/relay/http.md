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
| `GET /fetch/<broadcast>/<track>?group=N` | One group from the cache, the latest by default. Useful for catch-up and debugging. |
| `GET /certificate.sha256` | The TLS fingerprint, for pinning a self-signed dev certificate. |
| `GET /health` | `200 ok`, unauthenticated, for load balancers. |

```bash
curl http://localhost:4443/announced/demo
curl http://localhost:4443/fetch/demo/bbb.hang/catalog.json
```

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
process ran out of a resource `accept` needs. Host CPU and memory belong to a
node exporter.

### GET /nodes

This relay's view of the cluster: each visible node's URL, origin id, the
route its advertisement took, and the connections to it (with the same `conn`
id the logs use). It is best-effort correlation, not authenticated identity.
