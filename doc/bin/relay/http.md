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

A liveness and load-shedding probe for upstream load balancers.

- Returns `200` with the body `ok` when every configured threshold passes.
- Returns `503` with the body `overloaded`, followed by one line per breached threshold, otherwise.

With no thresholds configured it's a pure liveness probe (always `200`).
Each threshold is independent and only enforced when set, so you can mix and match.
It's unauthenticated so probes don't need a token.

```bash
curl -i http://localhost:4443/health
# HTTP/1.1 503 Service Unavailable
# overloaded
# cpu 82.1% exceeds 75%
# tx 612.0MB/s exceeds 500.0MB/s
```

Thresholds are read from the host via the cross-platform [`sysinfo`](https://crates.io/crates/sysinfo) crate.
Metrics are sampled in the background every `interval` seconds (default 2).

```toml
[web.health]
# Return 503 when global CPU usage exceeds this percentage. Accepts `75` or `75%`.
cpu = 75

# Return 503 when memory usage exceeds a percentage of total RAM (`80%`)
# or an absolute used-bytes amount (`32GB`, `32GiB`).
ram = "80%"

# Return 503 when aggregate received/transmitted throughput exceeds this rate.
# A unit is required; lowercase `b` is bits, uppercase `B` is bytes (`4Gb`, `500MB`).
# `/s` is always implied. Useful for shedding before you saturate the NIC.
rx = "4Gb"
tx = "500MB"

# Return 503 when the load average exceeds these limits. Each accepts a raw
# value (`6.0`) or a percentage of CPU cores (`80%`, i.e. a load of
# `0.8 * cores` — so `100%` is one runnable task per core). Unix only;
# these keys are rejected on Windows (which has no load average).
load1 = "8.0"
load5 = "80%"
load15 = "4.0"

# Seconds between metric samples. Defaults to 2, floored at 1.
interval = 2

# Defer the decision to another service. On each request the relay GETs this
# URL (5s timeout); a non-2xx response or an unreachable service counts as a
# breach (fail closed). Merges with the local thresholds above, so you can use
# it alone to simply proxy the verdict.
api = "http://localhost:9876/health"
```

Every key also has a CLI flag (`--web-health-cpu 75`, `--web-health-ram 80%`,
`--web-health-rx 4Gb`, `--web-health-tx 500MB`, `--web-health-load1 8.0`,
`--web-health-load5 80%`, `--web-health-load15 4.0`, `--web-health-interval 2`,
`--web-health-api http://localhost:9876/health`) and a matching
`MOQ_WEB_HEALTH_*` environment variable.

## See Also

- [Relay Configuration](/bin/relay/config) - Full config reference
- [Clustering](/bin/relay/cluster) - Multi-relay deployments
- [hang format](/concept/layer/hang) - Groups, keyframes, and container details
