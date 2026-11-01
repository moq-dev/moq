# [S] Probe scenario seeds randomize the media's start

## Goal

Each seed of the probe acceptance suite (`rs/moq-quic/src/tests/probe.rs`,
run nightly by `just rs quic-sim`) draws its own media start offset relative
to the handshake and the competing upload, so a scenario's mean across seeds
no longer carries one shared timing alignment as a bias.
The probing run and its baseline on a seed keep the same offset, so the pair
still differs only in the target.

## Plan

Decided 2026-10-31 (maintainer, via `/quest-plan`): plan it in m1 at [S].
Alternatives considered: m2, or drop it.
It came out of
[moq-dev/moq#7429](https://github.com/moq-dev/moq/pull/7429), which fixed
`competing_cubic`.

`run` in `probe.rs` takes the media's `start` from `pair.time` right after the
handshake and any warmup, and `frame_at(n)` schedules frame `n` at
`start + n / FPS`. The competitor's upload starts in the same loop iteration.
`entropy::reseed(scenario.seed)` varies keys, connection IDs, and the `Link`
impairments per seed, but not this phase, so every seed shares the same media
timing. With no probe at all, shifting competing_cubic's media by 7 µs moved
the 1024-seed mean p99 by −37 ± 6 ms, and 1 ms later moved it by
+19 ± 5 ms. That noise is the size of the probe budgets (40 ms of p99), so any
change that sends early in such a scenario is judged against an artifact of
the alignment.

- Draw the offset per seed, for example uniform over one frame interval
  (0 to 1/`FPS`, about 33 ms), and delay the media's first frame by it
  relative to the handshake and the competitor. That PR measured this version
  as a temporary harness change, and its result
  held (+2.3 ± 0.2 late frames, +0.1 ms p99).
- Draw it from the seed without shifting the endpoints' or links' later
  draws, such as from its own generator seeded from `scenario.seed`, so a
  seed's other randomness and its reproduction stay as they are.
- Keep `start`, the samples, and the rate steps measured from the media's
  start, so the rise, fall, and idle windows still mean the same thing.
- Re-run the acceptance suite over seeds 1-8 and a larger sweep. If a scenario's
  pass or fail on seeds 1-8 changes, report it with the paired spread rather
  than retuning budgets in this quest.

Public API: none. Wire: none.
