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

With [stats](/concept/stats) enabled, `/fetch` and `/announced` count as requests
on the token's tier and root. Fetched payload counts against its broadcast path;
neither endpoint counts as a connected session. Both endpoints return private,
non-cacheable responses. `/fetch` serves binary data, so save it to a file or
parse it as the track's format.

GET routes on the public router, including ones an embedder merges in, get a
wildcard CORS origin unless the route sets its own. Tokens sent over plain HTTP
are visible on the wire, so use HTTPS in production.

## Internal

```toml
[internal]
listen = "127.0.0.1:9101"
```

The listener is unauthenticated and must stay on loopback or a trusted private
network. Requests carrying an `Origin` header are refused. Ops tools must use
the listener's literal IP and port, or `localhost` on loopback; other hostnames
are refused to prevent DNS rebinding. A proxy must preserve that authority
rather than forward the caller's `Host`.

### GET /metrics

Prometheus text: payload bytes, frames, groups, announces, subscriptions, and
viewers split by `tier` and `role`; requests and sessions by `tier`; and
accept-loop counters per TCP listener. Payload counters accumulate for the
node's lifetime and leave out the stats feed's own traffic. Every counter fed by
sessions and requests, through the wire and control bytes below, counts only
while [stats](/bin/relay/config#stats) are enabled. Host CPU and memory belong
to a node exporter.

`moq_relay_inflated_bytes_total` counts the bytes the relay inflated from
[compressed tracks](/concept/compression) for readers that cannot carry
compression, once per group however many share the copy.

`moq_relay_wire_tx_bytes_total` and `moq_relay_wire_rx_bytes_total` sum the
per-session [transport counters](/concept/stats#sessions) by `tier`, including
internal sessions and the stats feed. Compare them with payload to spot
transport overhead and idle traffic. Transports that do not report them
contribute zero.

`moq_relay_control_tx_bytes_total` and `moq_relay_control_rx_bytes_total`
count control-message bytes by `tier` and `class` (`session`, `announce`, `track_info`, `subscribe`, `fetch`,
`probe`, `auth`, `goaway`, `other`) to spot control-heavy sessions. They
include message size prefixes but leave out media payload and transport
overhead such as QUIC framing, ACKs, and retransmissions.

A few worth alerting on:

- `moq_relay_accept_failures_total{class="exhausted"}`: the process ran out of
  a resource `accept` needs.
- `moq_relay_sessions_refused_total`, by `reason`. A rise in `unavailable`
  points at the auth server; a rise in `refused` at the credentials clients
  present. Each attempt counts, so a client that retries is counted again.
- `moq_relay_sessions_failed_total`, by `tier` and `kind` (`idle_timeout`,
  `transport`, `application`, `expired`): admitted sessions that ended
  abnormally. A clean close is not a failure. Reported only while
  [stats](/bin/relay/config#stats) are enabled.
- `moq_relay_stale_bytes_total` and friends: content dropped for drifting past
  a subscriber's latency budget.
- `moq_relay_draining_sessions`: sessions sent a GOAWAY during a
  [shutdown drain](/bin/relay/config#shutdown) that have not left yet.
- `moq_relay_panics_total`: bugs that closed one session instead of the
  relay. Each logs an error with the message and the session's span. A panic
  in a task every session depends on (the origin, the cache governor, the
  certificate reload) exits the relay instead.

With `--runtime-io-uring`, each QUIC worker also reports `moq_relay_uring_*`
counters under a `worker` label. Datagrams per enter and the SQE/CQE
histograms show whether syscalls carry useful work. Compare turns and
queue-pressure enters with throughput: flushing deferred work can remove
empty busy turns, so enters/turn alone does not grade efficiency. The
histograms include empty enters. A dead worker shows stuck zeros rather than
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
