# [L] A simulated viewer costs as little as its real session does

## Goal

The simulator's own overhead per viewer is measured and removed, so a laptop
holds as many real simulated-QUIC viewer sessions as it can, and the PR
records that ceiling: viewers per GB of memory and simulated seconds per wall
second, on a named laptop, for one broadcast at a stated bitrate. What a
viewer costs then is what a real session costs (QUIC, moq-net, the relay's
per-subscriber work), not the simulator.

## Plan

Measure first: profile a growing viewer count on the existing sim and break
each viewer's cost into the session itself and the simulator (link and timer
bookkeeping, per-task overhead, packet copies, crypto). Remove only the
simulator's part, recommended in this order, each kept if the profile says it
pays:

- a test-only null cipher for every session in the sim, building on
  `moq-quic`'s plaintext `fuzz` crypto; the cost a cipher adds per byte is
  already counted by the instruction benches, so the event report can state
  it beside its results rather than simulate it;
- a link model whose cost doesn't grow per packet in flight;
- no thread or OS resource per viewer.

Anything that cuts a real session's cost belongs in its own perf quest under
[the perf questline](/quest/m4/perf/README.md), not here, so the ceiling
keeps measuring the real thing. Keep the sim's changes behind the existing
`sim`/`test-support` features; nothing a release publishes changes shape.
