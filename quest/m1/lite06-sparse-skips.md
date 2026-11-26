# [L] A steady sparse-key broadcast stops growing over moq-lite-06

## Goal

The [steady run](/test/steady/README.md) passes on `moq-lite-06`, the version
clients negotiate by default: `moq-relay` and `moq export ts --passthrough`
level off after warm-up, and the export stops warning "media skipped
upstream" on every group. The Nightly runs the steady chain on lite-06 as
well as lite-07.

## Plan

Measured 2026-11-25 on `main` and earlier rounds of
[moq-dev/moq#9579](https://github.com/moq-dev/moq/pull/9579): over lite-06,
with 60 groups a second, the relay and the export each grew about 75 bytes a
group, while the import stayed flat. That growth came with sparse keys
([moq-dev/moq#8638](https://github.com/moq-dev/moq/pull/8638)), which skip
the keys between each group's start time.

With structural coalescing in #9579 (`Report::Final`), final `SUBSCRIBE_DROP`
runs and served groups now coalesce in `Record::reported`, which cuts the
relay and export growth in a 30-120 s lite-06 run to about +22 B/group (relay)
and +10 B/group (export). What still remains on lite-06:

- lite-06 has no `SUBSCRIBE_SKIP`, so `lite::Publisher::hole` sends each
  skipped run as `SUBSCRIBE_DROP`, which the subscriber reads as
  `StreamError::Old` (a drop, not `Hole::Skip`).
- Arrival-order readers with an unbounded budget still keep gaps between drops
  in `Record::delivered`, because only `Hole::Skip` is absorbed into
  `delivered` (a real drop must never hide a late-arriving group).
- The export's `Ordered` reader still returns each drop between groups as a
  gap, which `moq-mux` logs as "media skipped upstream" on every group.
- Full 900 s steady verification and a lite-06 lane in Nightly.

Options, to settle before building:

- Distinguish sparse-key inter-group gaps on lite-06 (or suppress the export's
  per-group warning on gaps that no group started in) and verify a full 900 s
  lite-06 steady run before adding it to Nightly.
- Leave lite-06 as-is until lite-07 is published and becomes the default.

Recommendation: verify the full 900 s lite-06 steady run (fixing any remaining
`delivered` growth if present), suppress the noisy per-group warning on gaps
that no group started in, and add lite-06 to the Nightly steady run. Keep any
version difference inside `moq-net`.

## Related

- [Relay soak](/quest/m4/relay-soak.md) - reuses the same growth judge for churn
