# moq-bench

A load generator for benchmarking a remote MoQ relay.

`moq-bench` opens many QUIC connections to a server and drives synthetic media
through them. Every knob is a `[min, max]` range that is rolled once per
connection, so a single config can describe a heterogeneous swarm (some
connections at 24fps, others at 60, etc).

## What it does

For a run, `moq-bench` establishes **A** connections. Each connection:

- publishes **B** broadcasts, each with a single track;
- subscribes to **C** other broadcasts discovered via announcements;
- produces **D** frames per second per track, each **E** bytes large;
- splits frames into groups of **F** frames each.

`--fanout <name>` selects a dedicated 1:N shape instead: the first connection
publishes one broadcast at `<name>/<run>/<fanout>`, and every remaining
connection subscribes to that exact broadcast. `--broadcasts` and `--subscribe`
are omitted in this mode because the roles are fixed.

The first frame of every group is a JSON keyframe describing the rolled
parameters (connection id, broadcast path, group sequence, fps, frame size,
group size, and a wall-clock timestamp), padded up to **E** bytes so the
configured frame size holds even when it is the only frame. The keyframe's own
header is the floor: an **E** below roughly 170 bytes sends the header at its
natural size, so configure **E** at 200 or above when exact sizes matter. The
remaining **F** frames in the group are zeroed. **F may be 0**, in which case
each group is a lone JSON keyframe: that is the chat shape (every message pays
a full group), and it also stresses the announce/subscribe control plane rather
than the data path.

To avoid a thundering herd at startup, connections and subscriptions are
staggered over a `--startup` ramp window instead of all firing at once.

## Stats

Every `--report` interval, `moq-bench` logs throughput (`send_mbps`/`recv_mbps`
and `send_fps`/`recv_fps`) plus delivery accounting for the subscribe side:

- `recv_groups`: cumulative groups received across all subscriptions.
- `lost_groups`: cumulative groups that never arrived.
- `loss`: `lost_groups` as a percentage.
- `latency_p50_ms` / `latency_p90_ms` / `latency_p99_ms` / `latency_max_ms`:
  cumulative one-way delivery latency from each group's keyframe timestamp.

Subscribers read groups in arrival order (out-of-order included) and track each
subscription's sequence span. A span wider than the count received means groups
in between were skipped, so loss reflects dropped groups rather than QUIC packet
loss (which the transport already repairs). The newest group is the live frontier
and is excluded from the count: groups just behind it may still be in flight, so a
gap is only blamed once a higher group confirms it was truly skipped. The JSON
keyframe at the start of each group is parsed back to recover the publisher's
shape, so a subscriber works against peers it didn't publish itself.

## Usage

```bash
# Roll the dice with the built-in defaults (1 connection, 1 broadcast, 30fps).
moq-bench --client-connect https://relay.example.com

# Use a preset, overriding the target and connection count on the CLI.
moq-bench --file rs/moq-bench/config/hd.toml \
  --client-connect https://relay.example.com \
  --connections 500
```

CLI flags always win over the TOML file, matching `moq-relay`. Every range
accepts a scalar (`--fps 30`), a `min:max` string (`--fps 24:60`), or a TOML
table (`fps = { min = 24, max = 60 }`).

### Key flags

| Flag | Var | Meaning |
|---|---|---|
| `--connections` | A | Connections to establish (rolled once for the run) |
| `--broadcasts` | B | Broadcasts published per connection |
| `--subscribe` | C | Peer broadcasts each connection watches |
| `--fps` | D | Frames per second per track (0 = idle) |
| `--frame-size` | E | Bytes per frame |
| `--group-size` | F | Zeroed frames per group after the keyframe |
| `--fanout` | | Publish one named broadcast and point every other connection at it |
| `--startup` | | Ramp window for staggering connections/subscriptions |
| `--duration` | | Stop after this long (runs until interrupted otherwise) |
| `--report` | | How often to log throughput stats |
| `--output` | | Also append the stats as JSON lines to this file |

Client TLS/QUIC flags (`--client-tls-disable-verify`, `--client-bind`, ...) come from
`moq-native` and behave the same as in `moq-cli` and `moq-relay`.

## Presets

The `config/` directory has a few starting points:

- `hd.toml`: high-bitrate HD video (24-60fps, several Mbps per track).
- `sd.toml`: standard-definition video with more viewers per publisher.
- `audio.toml`: small, frequent frames with short groups (Opus-like).
- `announce.toml`: many broadcasts, near-zero media, to stress announcements.
- `chat.toml`: one active chat channel with 2,000 viewers, one group per
  message, and every viewer targeting that named broadcast in the same process.

## Machine-readable output

`--output stats.jsonl` mirrors every report interval to a file as one JSON
line, each stamped with `timestamp_ms`. The frame, byte, and group counters are
cumulative and monotonic, so a consumer diffs successive lines to compute
rates, the same convention as `moq-stats` frames. `connections`, `broadcasts`,
and `subscriptions` are live gauges that fall as work ends; diff only the
counters.

Latency is measured from wall-clock milliseconds embedded by the publisher.
Keep separate publisher and subscriber hosts NTP-synchronized. Samples where
the receiver clock is behind the sender are omitted and counted in
`latency_clock_skew`; values at or above 60 seconds share the last percentile
bucket while `latency_max_ms` preserves the exact maximum.

## Host-side sampling: moq-bench-host

`moq-bench` measures the load it generates; `moq-bench-host` measures what that
load costs. It runs on the host of the process under test, production included:
it only reads `/proc` (Linux-only), needs no privileges beyond visibility of the
target process, and never touches the process itself (no ptrace, no perf, no
signals).

```bash
# On the relay host: sample the relay once per second, forever, to stdout.
moq-bench-host --name moq-relay

# Bounded run to a file, with the per-thread breakdown.
moq-bench-host --name moq-relay --interval 1s --duration 5m \
  --threads --output host.jsonl
```

Each line carries cumulative CPU seconds (`cpu_user`, `cpu_system`), RSS, and
context switches summed across threads (`ctx_voluntary`, `ctx_involuntary`;
an exited thread's contribution is retained, so the counters stay monotonic),
plus the host's total busy CPU seconds (`host_cpu_busy`) and core count so the
process's share of the machine is computable. `--threads` adds a per-thread
breakdown including the core each thread last ran on, which is how to verify
pinning once the relay grows a thread-per-core mode.

## Measuring CPU cost

Run the load from one machine and the sampler on the relay's host, then join
the two JSONL files on `timestamp_ms` over the same steady-state window. Skip
the `--startup` window while connections ramp. `timestamp_ms` is wall clock from two different hosts,
so keep both NTP-synced; the join only has to agree on the window boundaries,
since every rate comes from deltas within a single file:

- CPU per connection: `(delta cpu_user + delta cpu_system) / delta seconds / connections`.
- CPU per published message: `(delta cpu_user + delta cpu_system) / delta frames_sent`.
  This charges each message once, including its whole fanout, so it is the
  number that answers "what does one chat message cost the relay".
- CPU per delivered copy: same numerator over `delta frames_recv`, when the
  per-subscriber cost is the question. Do not sum the frame counters: the
  fan-out publisher counts each message once while subscribers count every copy.
- Context switches per message: either shape, with `delta ctx_voluntary + delta ctx_involuntary`
  as the numerator.

Comparisons are only meaningful between runs on the same hardware with the same
preset. Pin the relay build (`--version` appears in the logs) and record it next
to the results.
