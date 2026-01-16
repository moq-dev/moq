# Optional observability stack (Grafana + Prometheus + OTel Collector)

This folder is **optional tooling** to help reviewers/operators quickly spin up the minimum infra to view metrics emitted by this PR:

- Relay metrics: served at `http://localhost:4443/metrics`
- Browser metrics: exported via OTLP/HTTP to `http://localhost:4318/v1/metrics`

## Start

```bash
cd observability
docker compose up -d
```

- **Grafana**: `http://localhost:3050` (anonymous admin enabled)
- **Prometheus**: `http://localhost:9090`

## Verify data is flowing

### Prometheus targets
Open Prometheus targets page and confirm both are **UP**:

- `otel-collector` (scrapes `otel-collector:8889`)
- `moq-relay` (scrapes `host.docker.internal:4443/metrics`)

### Example queries
In Prometheus or Grafana Explore:

- `moq_relay_active_sessions`
- `moq_relay_app_bytes_sent_total`
- `moq_relay_app_bytes_received_total`
- `moq_client_connections_total`
- `moq_client_startup_time_seconds_count`

## Linux / WSL2 notes

The Prometheus config scrapes the relay using `host.docker.internal:4443`. The compose file includes:

- `extra_hosts: ["host.docker.internal:host-gateway"]`

If your Docker setup doesn’t support `host-gateway`, edit `observability/prometheus.yml` and replace the target with your host IP.

# MoQ Observability Stack

Real-time monitoring and debugging for Media over QUIC (MoQ) streaming infrastructure.

## Overview

This observability stack provides end-to-end visibility into the MoQ streaming pipeline, from client players to relay servers. It collects metrics, traces, and logs to help you:

- **Monitor** active viewers, streams, and connections in real-time
- **Debug** playback issues with per-session traces
- **Analyze** client experience (buffer health, startup time, quality switches)
- **Alert** on performance degradation or failures

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                              DATA SOURCES                                       │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│   ┌──────────────┐                              ┌──────────────┐                │
│   │   Browser    │                              │  MoQ Relay   │                │
│   │   Player     │                              │   (Rust)     │                │
│   │              │                              │              │                │
│   │ ┌──────────┐ │                              │ ┌──────────┐ │                │
│   │ │  OTel JS │ │                              │ │ OTel SDK │ │                │
│   │ │   SDK    │ │                              │ │  (Rust)  │ │                │
│   │ └────┬─────┘ │                              │ └────┬─────┘ │                │
│   └──────┼───────┘                              └──────┼───────┘                │
│          │                                             │                        │
│          │ OTLP/HTTP                                   │ OTLP/gRPC              │
│          │ (metrics, traces)                           │ (metrics, traces)      │
│          │                                             │                        │
└──────────┼─────────────────────────────────────────────┼────────────────────────┘
           │                                             │
           ▼                                             ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                           COLLECTION LAYER                                      │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│                      ┌────────────────────────┐                                 │
│                      │   OpenTelemetry        │                                 │
│                      │   Collector            │                                 │
│                      │                        │                                 │
│                      │  ┌─────────────────┐   │                                 │
│                      │  │ OTLP Receivers  │   │  ← Receives all telemetry       │
│                      │  │ (gRPC + HTTP)   │   │                                 │
│                      │  └────────┬────────┘   │                                 │
│                      │           │            │                                 │
│                      │  ┌────────▼────────┐   │                                 │
│                      │  │ Batch Processor │   │  ← Batches for efficiency       │
│                      │  └────────┬────────┘   │                                 │
│                      │           │            │                                 │
│                      │  ┌────────▼────────┐   │                                 │
│                      │  │    Exporters    │   │  ← Routes to backends           │
│                      │  └─────────────────┘   │                                 │
│                      └───────────┬────────────┘                                 │
│                                  │                                              │
│            ┌─────────────────────┼─────────────────────┐                        │
│            │                     │                     │                        │
│            ▼                     ▼                     ▼                        │
│     ┌───────────────┐    ┌───────────────┐    ┌───────────────┐                 │
│     │  Prometheus   │    │    Tempo      │    │     Loki      │                 │
│     │  (Metrics)    │    │   (Traces)    │    │    (Logs)     │                 │
│     └───────────────┘    └───────────────┘    └───────────────┘                 │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
                                     │
                                     ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                          VISUALIZATION LAYER                                    │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│                         ┌─────────────────────┐                                 │
│                         │      Grafana        │                                 │
│                         │                     │                                 │
│                         │  ┌───────────────┐  │                                 │
│                         │  │  Dashboards   │  │                                 │
│                         │  │ - MoQ Overview│  │                                 │
│                         │  | - MoQ Pipeline│  │                                 │
│                         │  │ - Node Stats  │  │                                 │
│                         │  └───────────────┘  │                                 │
│                         │                     │                                 │
│                         │  http://localhost:3050                                │
│                         └─────────────────────┘                                 │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

## Data Flow

### 1. Client (Browser) → OTel Collector → Prometheus/Tempo

```
Browser Player
     │
     │ Records metrics:
     │  - Buffer length (how much video is buffered)
     │  - Startup time (time to first frame)
     │  - Quality switches (adaptive bitrate changes)
     │  - Connection type (WebTransport vs WebSocket)
     │
     ▼
OpenTelemetry JS SDK (in browser)
     │
     │ Exports via OTLP/HTTP every 10 seconds
     │ POST http://localhost:4318/v1/metrics
     │ POST http://localhost:4318/v1/traces
     │
     ▼
OTel Collector (port 4318)
     │
     ├──► Prometheus (port 9090) ──► Grafana dashboards
     │    Metrics like:
     │      moq_client_buffer_length_seconds
     │      moq_client_startup_time_seconds
     │      moq_client_connections_total{transport="websocket"}
     │
     └──► Tempo (port 3200) ──► Grafana trace explorer
          Traces for debugging individual sessions
```

### 2. Relay (Rust) → OTel Collector → Prometheus/Tempo

```
MoQ Relay Server
     │
     │ Records metrics:
     │  - Active streams/subscribers/connections
     │  - Bytes sent/received (bandwidth)
     │  - Connection lifecycle events
     │  - QUIC stats (RTT, packet loss) - when using WebTransport
     │
     ▼
OpenTelemetry Rust SDK
     │
     │ Exports via OTLP/gRPC every 10 seconds
     │ grpc://localhost:4317
     │
     ▼
OTel Collector (port 4317)
     │
     ├──► Prometheus (port 9090)
     │    Metrics like:
     │      moq_relay_active_subscribers
     │      moq_relay_active_streams
     │      moq_relay_bytes_sent_total
     │
     └──► Tempo (port 3200)
          Traces for connection lifecycle
```

### 3. Docker Logs → Alloy → Loki

```
Docker Containers (relay, etc.)
     │
     │ JSON structured logs with trace_id
     │
     ▼
Grafana Alloy (log collector)
     │
     │ Scrapes Docker container logs
     │ Parses JSON, extracts labels
     │
     ▼
Loki (port 3100)
     │
     └──► Grafana log explorer
          Can link logs ↔ traces via trace_id
```

## Quick Start

### 1. Start the observability stack

```bash
cd observability
docker compose up -d
```

### 2. Import dashboards

```bash
./import-dashboards.sh
```

### 3. Start the MoQ relay (from project root)

```bash
just dev
```

### 4. Access Grafana

Open http://localhost:3050 (login: admin/admin)

## Available Metrics

Metrics are split into two layers to align with MoQ's "relay stays dumb" philosophy:

### MoQ Layer (relay, media-agnostic)

The relay operates on MoQ-native units (objects, groups) without understanding media semantics.

| Metric | Type | Description |
|--------|------|-------------|
| `moq_relay_active_subscribers` | Gauge | Current viewer count |
| `moq_relay_active_streams` | Gauge | Current stream count |
| `moq_relay_active_connections` | Gauge | Current connection count |
| `moq_relay_connections_total` | Counter | Total connections over time |
| `moq_relay_bytes_sent_total` | Counter | Total bytes transmitted |
| `moq_relay_bytes_received_total` | Counter | Total bytes received |
| `moq_relay_app_bytes_sent_total` | Counter | App-level payload bytes sent (use for amplification; excludes retransmits) |
| `moq_relay_app_bytes_received_total` | Counter | App-level payload bytes received (use for amplification; excludes retransmits) |
| `moq_relay_objects_sent_total` | Counter | Total MoQ objects transmitted |
| `moq_relay_objects_received_total` | Counter | Total MoQ objects received |
| `moq_relay_groups_sent_total` | Counter | Total MoQ groups transmitted |
| `moq_relay_groups_received_total` | Counter | Total MoQ groups received |
| `moq_relay_cache_hits_total` | Counter | Experimental: “served without upstream work” (definition TBD; fanout-sensitive) |
| `moq_relay_cache_misses_total` | Counter | Experimental: “required upstream work” (definition TBD; fanout-sensitive) |
| `moq_relay_dedup_upstream_saved_total` | Counter | Upstream work avoided via subscription dedup (fanout-relay “cache effectiveness”) |
| `moq_relay_fanout` | Histogram | Effective fanout (currently derived periodically, not group-accurate) |
| `moq_relay_queue_depth` | Gauge | Pending objects in delivery queue |
| `moq_relay_drops_total` | Counter | Objects dropped (backpressure) |
| `moq_relay_errors_total` | Counter | Connection errors |

**Note on cache metrics in fanout relays:** In a Producer/Consumer fanout architecture, “cache hit rate” can be misleading unless it’s defined precisely (per-consumer delivery vs per-upstream work vs late-join retention). Prefer `moq_relay_dedup_upstream_saved_total` plus `moq_relay_app_bytes_{sent,received}_total` (amplification) until `cache_hits_total`/`cache_misses_total` are fully defined and wired.

**Labels:**
- `relay_instance`: Relay identifier
- `namespace`: Stream namespace
- `region`: Deployment region

### Hang Layer (media-aware, client-side)

Media-specific metrics are collected in the browser by the hang player, not the relay.

**Client Experience (CMCD-aligned):**

| Metric | Type | Description |
|--------|------|-------------|
| `moq_client_buffer_length_seconds` | Histogram | Video buffer length in seconds |
| `moq_client_startup_time_seconds` | Histogram | Time to first frame |
| `moq_client_latency_seconds` | Histogram | Latency to live edge |
| `moq_client_bitrate_bps` | Histogram | Current playback bitrate |
| `moq_client_quality_switches_total` | Counter | Quality/bitrate switches |
| `moq_client_connections_total` | Counter | Connections by transport type |
| `moq_client_rebuffer_count_total` | Counter | Rebuffering events |

**Decode/Render Metrics:**

| Metric | Type | Description |
|--------|------|-------------|
| `moq_client_frames_decoded_total` | Counter | Successfully decoded frames |
| `moq_client_frames_dropped_total` | Counter | Dropped frames (congestion) |
| `moq_client_keyframe_interval_seconds` | Histogram | Time between keyframes |
| `moq_client_decode_time_seconds` | Histogram | Frame decode latency |
| `moq_client_av_sync_drift_seconds` | Histogram | Audio/video sync drift |

**Labels:**
- `transport`: `webtransport` or `websocket`
- `codec`: e.g., `avc1.64001f`
- `track_type`: `video` or `audio`

## Dashboards

### MoQ Overview
The main dashboard showing:
- **Top row**: Key stats (viewers, streams, connections, transport distribution, startup time)
- **Client Experience**: Buffer health, startup time distribution, quality switches
- **Relay Performance**: Viewers over time, connection rate, bandwidth, objects/groups rate
- **Relay Effectiveness**: Cache hit rate, dedup savings, fanout distribution, queue depth
- **SLO Status**: Time-to-first-frame p95, end-to-end latency p95, stall ratio

### MoQ Pipeline
Detailed technical metrics for debugging.

### Node Exporter Full
System metrics (CPU, memory, disk, network) for the host.

## Ports Reference

| Service | Port | Purpose |
|---------|------|---------|
| Grafana | 3050 | Dashboards UI |
| Prometheus | 9090 | Metrics storage & queries |
| Tempo | 3200 | Trace storage |
| Loki | 3100 | Log storage |
| OTel Collector (gRPC) | 4317 | Relay telemetry ingestion |
| OTel Collector (HTTP) | 4318 | Browser telemetry ingestion |
| OTel Collector (Prometheus) | 8889 | Metrics export for scraping |
| Node Exporter | 9100 | System metrics |

## Configuration Files

| File | Purpose |
|------|---------|
| `docker-compose.yml` | All observability services |
| `otel-collector-config.yaml` | OTel Collector pipelines |
| `prometheus.yml` | Prometheus scrape config |
| `tempo-config.yaml` | Tempo trace storage |
| `alloy-config.alloy` | Log collection config |
| `grafana/provisioning/` | Datasources & dashboards |

## Troubleshooting

### No client metrics appearing

1. Check browser console for `[Observability] Initialized`
2. Verify CORS: Browser should successfully POST to `localhost:4318`
3. Check OTel Collector logs: `docker logs observability-otel-collector-1`

### No relay metrics appearing

1. Check relay logs for OTel initialization
2. Verify Prometheus is scraping: http://localhost:9090/targets
3. Query directly: `curl 'http://localhost:9090/api/v1/query?query=moq_relay_active_streams'`

### Dashboard shows "No data"

1. Verify time range (top right) is recent
2. Check datasource connection in panel edit mode
3. Run `./import-dashboards.sh` to re-import

### WebSocket fallback instead of WebTransport

Check `moq_client_connections_total` by transport label:
```promql
sum by (transport) (moq_client_connections_total)
```

If all connections are `websocket`, WebTransport may not be supported or configured.

## Design Decisions

### Why no session_id in metrics?

Session IDs are **high-cardinality** and would cause Prometheus to run out of memory with many users. Instead:

| Signal | Per-Session | Use Case |
|--------|-------------|----------|
| Metrics | ❌ Aggregates | "How many users? Avg latency?" |
| Traces | ✅ Per-session | "Debug THIS user's issue" |
| Logs | ✅ Per-session | "What happened to session X?" |

### Why OpenTelemetry?

- **Vendor-neutral**: Switch backends without code changes
- **Standard protocol**: OTLP is widely supported
- **Single SDK**: Metrics, traces, logs in one library
- **Future-proof**: CNCF graduated project

### Why this stack (Prometheus/Tempo/Loki/Grafana)?

- **All Grafana ecosystem**: Seamless integration
- **Proven at scale**: Used by major companies
- **Open source**: No vendor lock-in
- **Rich querying**: PromQL, TraceQL, LogQL

### Browser OTLP in Production

In development, browsers send telemetry directly to `localhost:4318`. In production, browser OTLP must be sent to a reachable endpoint to avoid CORS and network reachability issues:

| Option | Description |
|--------|-------------|
| **Same-origin proxy** | Add `/otel` path on the relay that proxies to the collector |
| **Dedicated ingress** | Deploy collector with proper CORS headers on a public endpoint |
| **Edge collector** | Run collector at CDN edge (Cloudflare Workers, etc.) |

**Example nginx proxy configuration:**
```nginx
location /otel/ {
    proxy_pass http://otel-collector:4318/;
    proxy_set_header Host $host;

    # CORS headers for browser requests
    add_header 'Access-Control-Allow-Origin' '*';
    add_header 'Access-Control-Allow-Methods' 'POST, OPTIONS';
    add_header 'Access-Control-Allow-Headers' 'Content-Type';
}
```

**Client configuration for production:**
```typescript
initObservability({
  // Use same-origin path to avoid CORS issues
  otlpEndpoint: `${window.location.origin}/otel`,
  serviceName: "moq-client",
});
```

### Trace/Log/qlog Correlation

All telemetry signals (traces, logs, qlog) share a common `connection_id` for correlation:

| Signal | Location | How to correlate |
|--------|----------|------------------|
| **Traces** | Tempo | Filter by `connection_id` span attribute |
| **Logs** | Loki | Filter by `connection_id` field in JSON logs |
| **qlog** | File system | qlog files are named `qlog/{connection_id}/trace.json` |

**Correlation workflow:**

1. Find an interesting trace in Grafana/Tempo
2. Copy the `connection_id` from the span attributes
3. Search logs in Loki: `{service="moq-relay"} | json | connection_id="conn-123"`
4. If deeper QUIC debugging is needed, check the `qlog_path` attribute in the trace to find the qlog file

**Enabling qlog for QUIC forensics:**
```bash
# Enable qlog with 10% sampling
MOQ_QLOG_ENABLED=true MOQ_QLOG_SAMPLE_RATE=0.1 just dev
```

qlog files can be visualized with [qvis](https://qvis.quictools.info/) for detailed QUIC protocol analysis.

## Next Steps

To extend this observability setup:

1. **Add alerting**: Define rules in `alerts.yml`
2. **Add broadcast labels**: Track per-stream metrics
3. **Enable QUIC stats**: When WebTransport is working
4. **Add geographic labels**: Track viewer distribution
5. **Set up dashboards for specific use cases**: Live events, VOD, etc.
