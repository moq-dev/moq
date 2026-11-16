# [M] The relay's chat and fanout CPU growth since release is explained before the cut

## Goal

The relay's extra CPU per delivered frame on `main` against `release` in the
`chat` and `fanout` workloads has a named cause, and the cut ships with it
either fixed or written down as the price of a feature, in
`.github/release-perf.md` beside the
[memory finding](/.github/release-perf.md#relay-memory-2026-11-16). Nothing
here changes an API or the wire.

## Plan

Found 2026-11-16 by [relay memory](https://github.com/moq-dev/moq/pull/8943),
whose pairing reported CPU beside RSS. Placed in m2, like the memory quest,
so the cut waits on it: the 2026-11-05 pairing said `main` used less relay
CPU in every workload, and 0.22 should not be announced on that claim while
two workloads now say the opposite.

Facts:

- Five alternating rounds of `just bench-relay origin/release quic`
  (`release` at `ca3b801a6c`, `main` at `6488ce6b87`, host load 24 to 28 on
  32 cores): `cpu_us_per_frame` +43% in `chat` (range +24% to +59%) and +35%
  in `fanout` (range +3% to +65%), every pair slower; `video` -24%. Delivered
  frames per second matched. The heap-profile controls on 8 Tokio workers
  agree: `chat` relay CPU 1.40 to 1.52 cores on `release`, 1.96 to 2.15 on
  `main`.
- `main`'s relay allocates 2.3 times the bytes per second in `chat` (about
  300 MiB/s against 133), though fewer allocations. Half of it is
  `moq_quic::connection::slots::Slots::insert`: a connection that has every
  packet acknowledged before it sends the next frees its 64-slot chunk
  (about 8 KiB of `Option<SentPacket>`) and allocates a fresh one for the
  next packet ([moq-dev/moq#6936](https://github.com/moq-dev/moq/pull/6936)).
  That is one 8 KiB allocation per packet in `chat`. A lead, not a measured
  cause: count instructions before and after keeping one spare chunk.
- The lifetime `loss_pct` also rose (about 1.5% against 0.1%), all of it
  during the startup ramp: `groups_lost` stops growing once the load is
  ready, so steady delivery is unchanged. It may be groups `main` now
  reports dropped as a subscriber joins; confirm that before reading it as
  a regression.

For the PR: attribute the CPU with `just bench-profile cpu chat 8
tokio-workers origin/release`, counted in instructions as the
[performance audit's Method](/quest/m3/audit-perf.md#method) does, by
owner: transport, session, origin, other. Fix what is accidental; record a
feature's cost with its PR.

## Waiting

- Maintainer: whether the cut waits on this. Recommended: yes, as it does on
  memory, since the release comparison now claims the opposite of what
  `main` measures. Otherwise move it under [m3](/quest/m3/README.md) with
  `quest mv`, where the performance audit's release run picks it up after
  the cut.

## Related

- [Performance audit](/quest/m3/audit-perf.md) - reruns the release pairing after the cut
