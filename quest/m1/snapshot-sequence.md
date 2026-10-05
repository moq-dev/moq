# [S] Snapshot producers choose their group sequence

## Goal

`moq_json::snapshot::Producer`, and moq-mux's `catalog::Producer` over it, let
the caller seed or choose each snapshot group's sequence number instead of
always taking `append_group()`'s next one. A producer that is recomposed or
restarted under the same track name can then publish sequences that keep
increasing across restarts, such as `max(last + 1, now_ms)`, so a subscriber or
relay holding the old run's groups never waits for the counter to catch up.
Every snapshot group starts with a full snapshot, so gaps between sequences
are legal.

## Plan

Decided 2026-10-05 in moq.pro's follow-ups (approved by the maintainer): the
consumer is moq.pro's edge-composed overlay catalog, which recomposes a
`catalog.pro` whenever its source or services change and must not restart at
group 0.

- `write_snapshot` (`rs/moq-json/src/snapshot/producer.rs`) opens each group
  with `self.inner.append_group()`. Add a way to pick the sequence: a seed in
  `Config`, a caller-supplied allocator, or both. Pick the smallest shape that
  covers a wall-clock seed and a shared allocator, and record why. A sequence
  at or below the last one written is refused, never reordered.
- `moq-stats` already solved this for its own snapshots with a producer-wide
  allocator seeded from the wall clock (moq#4739, moq#4810), by wrapping the
  encoder in its own `Snapshot` writer (`rs/moq-stats/src/produce.rs`). Move
  that onto the shared producer and delete the duplicate writer, so one path
  numbers every snapshot track.
- `catalog::Producer` (`rs/moq-mux/src/catalog/producer.rs`) builds two
  snapshot producers (`hang` and `hangz`). Each catalog update picks one
  sequence and writes it to both, so their groups stay paired; a test checks
  the pair shares a sequence.
- JS: `js/json`'s snapshot producer gets the same option if it has the same
  `append_group` shape; mirror the name.
- Tests: a seeded producer starts at the seed; a recreated producer seeded
  from `max(last + 1, now_ms)` resumes past a cached group; a non-increasing
  choice is refused; a subscriber reading across a gap gets a full snapshot.

Public API: a sequence option on `moq_json::snapshot::Producer` and
`moq_mux::catalog::Producer` (and the JS mirror). Wire: none.

## Related

- [Coalesce dynamic tracks](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - preserves sequences when a dynamic track's producer is replaced inside moq-net; this covers producers the caller recreates
- [Stats epochs](/quest/m0/broadcast-epoch/stats-epoch.md) - stats' other answer to restarts
- [moq.pro: overlay catalog](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/overlay/catalog.md) - the recomposing producer that needs it
