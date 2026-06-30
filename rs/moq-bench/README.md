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

The first frame of every group is a JSON keyframe describing the rolled
parameters (connection id, broadcast path, group sequence, fps, frame size,
group size, and a wall-clock timestamp). The remaining **F** frames in the group
are zeroed. **F may be 0**, in which case each group is a lone JSON keyframe,
which is useful for stressing the announce/subscribe control plane rather than
the data path.

To avoid a thundering herd at startup, connections and subscriptions are
staggered over a `--startup` ramp window instead of all firing at once.

## Stats

Every `--report` interval, `moq-bench` logs throughput (`send_mbps`/`recv_mbps`
and `send_fps`/`recv_fps`) plus delivery accounting for the subscribe side:

- `recv_groups`: cumulative groups received across all subscriptions.
- `lost_groups`: cumulative groups that never arrived.
- `loss`: `lost_groups` as a percentage.

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
moq-bench --url https://relay.example.com

# Use a preset, overriding the target and connection count on the CLI.
moq-bench --file rs/moq-bench/config/hd.toml \
  --url https://relay.example.com \
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
| `--startup` | | Ramp window for staggering connections/subscriptions |
| `--duration` | | Stop after this long (runs until interrupted otherwise) |
| `--report` | | How often to log throughput stats |

Client TLS/QUIC flags (`--client-tls-disable-verify`, `--client-bind`, ...) come from
`moq-native` and behave the same as in `moq-cli` and `moq-relay`.

## Presets

The `config/` directory has a few starting points:

- `hd.toml`: high-bitrate HD video (24-60fps, several Mbps per track).
- `sd.toml`: standard-definition video with more viewers per publisher.
- `audio.toml`: small, frequent frames with short groups (Opus-like).
- `announce.toml`: many broadcasts, near-zero media, to stress announcements.
