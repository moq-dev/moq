# [S] Relay peer links default to release's 64 MiB windows

## Goal

`[limits.peer.quic]` defaults to a 64 MiB `receive_window` and `send_window`,
the connection window every session had on the release line, so a relay
upgraded from release carries the same peer-link traffic: about 2 Gbps at a
250 ms RTT instead of the 450 Mbps that main's 16 MiB carries. Peer memory
stays a fixed, configured bound (peers × 64 MiB).

Non-goals: client (`[quic]`) windows, and a peer stream window (see Plan).

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the main → release
flow-control review):

- ✅ 64 MiB, release parity (rejected: 32 MiB, below release; a value picked by
  a sim sweep, which needs a throughput target nobody named). Release's
  `moq-tokio` sized it for "several Gbps at intercontinental RTTs".
- ✅ m1, and the cut doesn't wait on it (rejected: the cut waits). It ships
  with the cut if it lands first, else in a `release` patch.
- ✅ Peers keep the 1 MiB stream window from `[quic]`, as
  [#7624](https://github.com/moq-dev/moq/pull/7624)'s decision 11 settled
  (rejected: a `[limits.peer.quic] stream_receive_window`). Steady live media
  needs a group's bitrate, far under 1 MiB per RTT. The window bites only on a
  track above about 30 Mbps on a 250 ms link, and on a backlog burst: an edge
  subscribing upstream mid-group gets the group's cached 1 to 2 s at 1 MiB per
  RTT instead of line rate. Measure that burst on a cross-region join (for
  example a 16 Mbps track joined mid-group across a 250 ms peer link) and
  report the added join latency in the PR. Plan a peer stream window as its own
  quest only if it matters.

Update the defaults in `rs/moq-relay/src/limits.rs`, `doc/bin/relay/config.md`,
and the `[limits.peer.quic]` entry in `rs/moq-relay/MIGRATE.md`, which today
tells users to restore larger windows themselves.

Caused by [#7624](https://github.com/moq-dev/moq/pull/7624), which lowered
the peer windows from 640 MiB to 16 MiB (its decision 11).
