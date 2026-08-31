# [M] moq-uring: p50 latency collapses at high connections per worker; the UDP pools are fixed at 64…

## Goal

Implement and verify the behavior tracked in [#3119](https://github.com/moq-dev/moq/issues/3119)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Benchmarking the io\_uring relay against the tokio one on `dev` (`fc57e0175`), the io\_uring path holds latency parity up to ~100 connections per worker and then falls off a cliff, while the tokio worker path degrades gracefully.

Video fan-out (1 publisher, N-1 subscribers, 60fps, 4 KB frames, 60-frame groups), `--runtime-workers 4`, loopback, AMD Ryzen 7 5800X, relay pinned to cores 0-3:

| connections | mode | relay cores | Mbps | p50 ms | p90 ms | RSS MB |
|---|---|---|---|---|---|---|
| 401 | tokio workers | 1.246 | 768.0 | 4 | 6 | 494 |
| 401 | io\_uring | 1.239 | 767.9 | 4 | 6 | 85 |
| 801 | tokio workers | 2.451 | 1535.9 | 14 | 18 | 961 |
| 801 | io\_uring | 2.776 | 1535.9 | 27 | 33 | 149 |
| 1601 | tokio workers | 3.511 | 2449.6 | **122** | **181** | 1933 |
| 1601 | io\_uring | 3.791 | 2378.7 | **566** | **893** | 811 |

At 1601 connections over 4 workers (400 per worker) io\_uring's p50 is 4.6x the tokio path's.

#### Mechanism

`udp::Config::default()` fixes the per-socket pools at `tx_buffers: 64` and `rx_buffers: 16`, and there is one socket per worker. `Connection::flush` acquires exactly one `TxBuf`, stages at most one GSO train into it, and returns `Poll::Pending` when `poll_acquire` finds the pool empty. So a worker's entire send concurrency is 64 buffers no matter how many connections it serves: at 400 connections per worker, connections serialize behind the pool and the wait shows up as queueing delay.

`rs/moq-relay/src/uring.rs` builds that config and only ever sets `gso`:

```rust
let mut udp = moq_uring::udp::Config::default();
udp.gso = quic.gso.unwrap_or(true);
```

The fields are `pub` on `udp::Config`, but nothing in the relay scales them with the expected connection count and no operator flag reaches them.

#### Confirmation

Raising the pools to `tx_buffers: 1024, rx_buffers: 256` and re-running restores latency parity, and delivery goes from 1583 to the full 1600 groups/s:

| 1601 connections | Mbps | p50 ms | p90 ms | groups/s | RSS MB |
|---|---|---|---|---|---|
| io\_uring, default pools | 2378.7 | 566 | 893 | 1583.1 | 811 |
| io\_uring, 1024/256 | 1475.2 | **117** | **180** | **1600.1** | 470 |
| tokio workers | 2449.6 | 122 | 181 | 1600.0 | 1933 |

So the pool depth is the knob, but it is not a free win: deeper pools cost throughput here (1475 vs 2379 Mbps) and 4 x 64 KB x 1024 is 256 MB of send staging across the group. At 801 connections the same change is roughly neutral (p50 27 -> 26 ms, 1536 -> 1533 Mbps).

#### Suggestion

The defaults are a reasonable floor for a handful of connections and clearly wrong for hundreds. Some scaling policy tied to expected connections per worker, plus a way for an operator to set it, would let the mode hold its latency at the connection counts a relay actually sees. The throughput regression above suggests the right answer is not simply a bigger constant.

Numbers are single runs per cell (the 201-connection rows elsewhere were n=2 and repeatable to ~1%), same host for relay and load generator with the generator pinned to the other cores, `net.core.{r,w}mem_max` at 4 MiB (the relay warns it wanted more, equally in both modes).

## Closes

- [#3119](https://github.com/moq-dev/moq/issues/3119) - close this issue when the quest finishes
