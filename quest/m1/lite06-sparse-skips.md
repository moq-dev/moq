# [L] A steady sparse-key broadcast stops growing over moq-lite-06

## Goal

The [steady run](/test/steady/README.md) passes on `moq-lite-06`, the version
clients negotiate by default: `moq-relay` and `moq export ts --passthrough`
level off after warm-up, and the export stops warning "media skipped
upstream" on every group. The Nightly runs the steady chain on lite-06 as
well as lite-07.

## Plan

Measured 2026-11-25 on the fix for the lite-07 growth
([moq-dev/moq#9579](https://github.com/moq-dev/moq/pull/9579)): over lite-06,
with 60 groups a second, the relay and the export each grow about 75 bytes a
group, while the import stays flat. `main` grows the same, so the growth
predates that PR. It came with sparse keys
([moq-dev/moq#8638](https://github.com/moq-dev/moq/pull/8638)), which skip
the keys between each group's start time.

The cause: lite-06 has no SUBSCRIBE_SKIP, so `lite::Publisher::hole` sends
each skipped run as a SUBSCRIBE_DROP. Before lite-07, a drop carries no code,
and the subscriber reads every one as `StreamError::Old`, a final drop. The
relay's copy then holds a drop between every pair of groups. The reader
serving the export gives each one up as a miss and reports it dropped, which
`Fold::Final` folds only inside a span the track holds whole, where no group
can still land late, and lite-06 declares no spans. The
export's `Ordered` reader returns each one as a gap, which `moq-mux` logs.

Options, to settle before building:

- The lite-06 publisher sends nothing for a skip, and the lite-06 subscriber
  stops waiting on keys between groups some other way. It needs a way to know
  a key will never come, which lite-06 doesn't carry without a drop.
- A copy fed only by a subscription with no skip tells its readers that its
  drops are final, so they report them `Settled`, as the lite-06 publisher
  already does (`Report::Settled`: "given up on a version that has no skip").
  The `Ordered` reader passes a run it already returned as a gap the same
  way. This keeps the wire as it is. The export still warns on each gap.
- Leave lite-06 to grow until lite-07 is published and becomes the default.

Recommendation: the second, plus not warning on a drop of keys that no group
started in. It keeps the version difference inside `moq-net`.

## Related

- [Relay soak](/quest/m4/relay-soak.md) - reuses the same growth judge for churn
