# [M] Release the BBR correctness fixes

## Goal

Published MoQ consumers receive the seven corrected BBR behaviors through
immutable releases of the noq fork and its adapters. Fixes do not wait for
qmux, stream scheduling, or other unrelated QUIC features.

## Plan

The fixes are merged in moq-dev/noq `main` and ship as 1.4.0 of
`moq-noq-proto`, `moq-noq-udp`, `moq-noq`, and `web-transport-moq`, which
share one version:

- [moq-dev/noq#3](https://github.com/moq-dev/noq/pull/3) - BBR packet identity across QUIC spaces
- [moq-dev/noq#4](https://github.com/moq-dev/noq/pull/4) - each BBR ACK sample completes before the model uses it
- [moq-dev/noq#5](https://github.com/moq-dev/noq/pull/5) - the controller hears of application starvation before the next send
- [moq-dev/noq#6](https://github.com/moq-dev/noq/pull/6) - finished bandwidth probes age the max-bw window once and stop classifying losses as probe feedback
- [moq-dev/noq#7](https://github.com/moq-dev/noq/pull/7) - the first measured RTT replaces the 1ms estimate behind BBR's startup pacing rate
- [moq-dev/noq#8](https://github.com/moq-dev/noq/pull/8) - packets sent during ProbeRTT are application-limited, so its reduced rate cannot lower BBR's bandwidth model
- [moq-dev/noq#9](https://github.com/moq-dev/noq/pull/9) - a spurious loss episode restores the BBR model, window, and probe saved when the episode began

[moq-dev/noq#10](https://github.com/moq-dev/noq/pull/10) prepares the
release. It bumps the version and internal pins, and adds `CHANGELOG-MOQ.md`,
which names the parent (n0-computer/noq `1a26a8b0`, unchanged since 1.3.0),
the carried set, and each change's upstream status. Upstream has only #7's
root, [n0-computer/noq#802](https://github.com/n0-computer/noq/pull/802) (open);
the rest go upstream with the [upstream quest](/quest/m1/quic/upstream.md).
The API change is additive (new `Controller` callbacks that default to the
deprecated ones they replace), so the bump is minor.
`cargo semver-checks` against 1.3.0 is clean for all four crates.

Release steps, in order:

1. Merge moq-dev/noq#10.
2. On crates.io, confirm all four crates list moq-dev/noq `moq-release.yml`
   as a trusted publisher. `web-transport-moq` 1.3.0 was published apart from
   the other three.
3. Tag the merge commit: `git tag v1.4.0 <merge-sha> && git push origin v1.4.0`.
   `moq-release` checks the tag against the workspace version and runs
   `cargo publish --workspace`.
4. Here, in `Cargo.toml`, change `moq-noq-proto`, `moq-noq-udp`, and
   `web-transport-moq` from `"1.3"` to `"1.4"`, then run
   `cargo update -p moq-noq-proto -p moq-noq-udp -p moq-noq -p web-transport-moq`.
   The lock then holds the four at registry 1.4.0, and
   `cargo tree -d --workspace --all-features` shows no second copy.
5. Run `just check`, `just rs tokio-features`,
   `cargo clippy --locked -p moq-tokio -p moq-uring -p moq-relay --all-targets --all-features -- -D warnings`,
   `cargo nextest run --locked -p moq-tokio -p moq-uring -p moq-relay --all-features`,
   and the media check below with release builds of `moq-relay` and
   `moq-bench`.
6. The PR that lands the pin deletes this quest. release-plz picks up the
   dependency bump for `moq-tokio` and `moq-uring`.

The media check sends one 4.8 Mbps track (30 fps, 20 kB frames, 1 s groups)
to two subscribers through `moq-relay` over QUIC with the default Delay
family (BBR). The loopback runs in a rootless network namespace shaped by
netem. The WebSocket fallback is off, since it wins the race under loss, and
the relay log must show three QUIC sessions and none over WebSocket:

```bash
unshare -rn bash -c '
ip link set lo up
tc qdisc add dev lo root netem delay 20ms loss 2% rate 40mbit
moq-relay relay.toml &  # bind 127.0.0.1:4443, tls.generate, public auth, no iroh
sleep 1
moq-bench --connect http://localhost:4443 --connect-websocket-enabled=false \
  --fanout media --connections 3 --startup 1s --duration 60s --report 5s \
  --fps 30 --frame-size 20000 --group-size 29 --output stats.jsonl
kill %1'
```

Run it again with `delay 20ms loss 1% rate 16mbit`, just above the offered
load. Local runs against the fork through a `[patch.crates-io]` on
2026-09-25, two per cell, of about 71 MB offered:

| Path | 1.3.0 received | 1.3.0 latency p50 / p99 | 1.4.0 received | 1.4.0 latency p50 / p99 |
|---|---|---|---|---|
| 2% loss, 40 Mbps | 23-26 MB | 241-340 / 916-1076 ms | 69 MB | 54-55 / 131-141 ms |
| 1% loss, 16 Mbps | 53-55 MB | 115-120 / 334-414 ms | 57-61 MB | 130-132 / 293-376 ms |

The published crates are the same source, so a rerun against them should
match the 1.4.0 columns. The broader media-flow study and the Google comparison do not gate
these fixes.

## Related

- [Release the stack](/quest/m1/quic/release.md) - later feature releases follow the same packaging rules
- [BBR3 app-limited](/quest/m2/quic-bbr-app-limited.md) - broader media measurements after the corrected release
