# [S] Remove uncalled public items

## Goal

The uncalled items the dead-code audit found are gone: moq-cli's `nvenc` and
`nvdec` Cargo feature aliases, `moq_net::Version::uses_setup_negotiation`,
and moq-net's test helper `track::Subscriber::assert_error`. The
`AnnounceConsumer::assert_next_*` helpers stay: `model/origin.rs` calls them
since [#5678](https://github.com/moq-dev/moq/pull/5678).

## Plan

From [moq-dev/moq#6166](https://github.com/moq-dev/moq/pull/6166). Decided
2026-10-21 with the maintainer: m2.
Moved to m1 on 2026-10-22 (maintainer, from an external API audit): the
cut is breaking anyway, so this break ships with it instead of forcing
another breaking release later. Verify each still has no caller across
the repo, bindings included, before removing it; keep any that gained one.
The test helper lives in `model/track.rs`, which many open PRs churn, so
expect merge conflicts there.

Public API: breaking for moq-cli (features) and moq-net (`Version`), with a
`MIGRATE.md` entry in each. Wire: none.

## Related

- [Dead code and API surface audit](/quest/m2/audit-dead-code.md) - the next audit run, whose baselines record these findings
