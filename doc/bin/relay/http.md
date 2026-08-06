---
title: HTTP
description: HTTP endpoints exposed by moq-relay
---

# HTTP Endpoints

moq-relay exposes HTTP/HTTPS endpoints via TCP too.
These were initially added for debugging but are useful for many things, such as fetching old content.

## Configuration

The relay supports both HTTP and HTTPS, configured independently:

```toml
[web.http]
# Listen for unencrypted HTTP connections on TCP
listen = "0.0.0.0:80"

[web.https]
# Listen for encrypted HTTPS connections on TCP
listen = "0.0.0.0:443"
cert = "cert.pem"
key = "key.pem"
```

::: warning
HTTP is unencrypted, which means any [authentication tokens](/bin/relay/auth) will be sent in plaintext.
It's recommended to only use HTTPS in production.
:::

## Notable Endpoints

### GET /announced/\*prefix

Lists all announced broadcasts matching the given prefix.

```bash
# All broadcasts
curl http://localhost:4443/announced/

# Broadcasts under "demo/"
curl http://localhost:4443/announced/demo

# Specific broadcast
curl http://localhost:4443/announced/demo/my-stream
```

### GET /fetch/\*path

Fetches a specific group from a track, by default the latest group.
Useful for quick debugging without setting up a full subscriber, or for fetching old content.

The path is `/<broadcast>/<track>`, where the last segment is the track name and everything before it is the broadcast path.

```bash
# Get latest catalog from broadcast "demo/my-stream"
curl http://localhost:4443/fetch/demo/my-stream/catalog.json

# Get a specific video group from broadcast "demo/my-stream"
curl http://localhost:4443/fetch/demo/my-stream/video?group=42
```

::: tip
Use HTTP fetch for catch-up and historical data.
Use MoQ subscriptions for the live edge.
The two complement each other — HTTP is request/response, MoQ is pub/sub.
:::

### GET /certificate.sha256

Returns the SHA-256 fingerprint of the TLS certificate.
This is only useful for local development with self-signed certificates.

```bash
curl http://localhost:4443/certificate.sha256
# f4:a3:b2:... (hex-encoded fingerprint)
```

### GET /health

A liveness probe for upstream load balancers. Always returns `200` with the
body `ok\n` (a trailing newline). It's unauthenticated so probes don't need a
token.

```bash
curl -i http://localhost:4443/health
# HTTP/1.1 200 OK
# ok
```

Host overload monitoring (CPU, RAM, network, load average) belongs in a
separate process that watches the host, not in the relay itself. Point your
load balancer at that process for load shedding.

## Internal Operations Endpoints

The separate internal listener serves unauthenticated operational endpoints.
Bind it only to loopback or a trusted private network:

```toml
[internal]
listen = "127.0.0.1:9101"
```

It mirrors `/health`, exposes Prometheus counters at `/metrics`, and adds the
cluster topology endpoint below.

### GET /metrics

Prometheus text exposition of this node's own traffic counters (bytes, frames,
groups, subscriptions, viewers, sessions), split by `tier` and `role`.

Alongside them are the TCP listeners' accept-loop counters, which are how a node
that has stopped accepting connections says so:

```text
moq_relay_accept_failures_total{listener="web",class="connection"} 41
moq_relay_accept_failures_total{listener="web",class="exhausted"} 0
moq_relay_accept_failures_total{listener="web",class="unknown"} 0
moq_relay_accept_stalled_seconds{listener="web"} 0
```

One `listener` per socket that is actually configured: `web` for the HTTP/HTTPS
pair, `internal` for this listener, and `tcp` / `unix` for the qmux stream
listeners. A listener you have not configured is absent rather than zero, so a
stream-only relay publishes no `web` series at all. That is deliberate: a
permanent zero for a socket nobody opened reads as a watch that is passing, when
there is nothing there to watch.

`connection` counts connections that died before the relay dequeued them (a peer
reset, a firewall rule, a scanner): ordinary traffic on a public port, and never
a fault. A non-zero `exhausted` is the one worth paging on, and means the process
or the host ran out of a resource `accept` needs (file descriptors, kernel
memory) while connections were queued; `moq_relay_accept_stalled_seconds` is how
long that has been true, and only a successful accept clears it. `unknown` is an
errno the relay does not classify: worth a look, not an alert.

Alert on the `exhausted` counter rather than the gauge. A process out of
descriptors often cannot serve this scrape either, so the gauge tends to read
zero again by the time a scrape gets through, while the counter still shows the
jump.

Nothing here reports host CPU, memory, disk, or network; run a node exporter for
those. The QUIC listener has no accept counters at all: it multiplexes every
session over one UDP socket, so it never calls `accept` and cannot exhaust it.

### GET /nodes

Returns this relay's local view of cluster nodes. A node is included when it is
visible through the `.internal/origins` discovery namespace or has an
established outbound cluster connection. An inbound connection is attached
only after its SETUP origin id maps to one unique node advertisement. Sessions
without a unique match are omitted.

```json
{
  "nodes": [
    {
      "node": "https://relay-b.example/",
      "origin_id": "200",
      "announced": {
        "hops": ["200"],
        "hop_count": 1,
        "cost": 1
      },
      "connections": [
        { "id": 3, "direction": "inbound" }
      ]
    }
  ]
}
```

`announced` describes the selected route for the node's discovery
advertisement, not every physical link in the cluster. A node can be visible
without a direct connection, directly connected without an advertisement, or
both. Origin ids are decimal strings because the wire supports values larger
than JavaScript's precise integer range.

A connection `id` is the same id the session's log lines carry in their
`conn{id=...}` span, so a row here grep's straight to that session's logs.
Accepted sessions and outbound dials draw from one counter, so an id names
exactly one session. It is process-local and only lasts for that session, so
don't persist it or compare it across relays.

Inbound association is best-effort correlation, not authenticated node
identity. The SETUP origin id is self-declared, and `/nodes` associates it only
when one visible advertisement has the same origin id. Do not use this endpoint
for authorization or other security decisions.

## See Also

- [Relay Configuration](/bin/relay/config) - Full config reference
- [Clustering](/bin/relay/cluster) - Multi-relay deployments
- [hang format](/concept/layer/hang) - Groups, keyframes, and container details
