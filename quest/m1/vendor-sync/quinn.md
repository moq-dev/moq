# [S] moq-quic has every quinn fix it needs

## Goal

moq-quic, moq-sock's `udp`, moq-tokio's `quic::driver`, and moq-congestion's
BBR3 carry every
security and correctness fix quinn has landed since the last sync, each
ported with its regression test or recorded as not applying. The fork
ledger in `rs/moq-quic/README.md` says how far upstream was checked.

## Plan

Recurring child of the [vendor sync](/quest/m1/vendor-sync/README.md), which
says how each run works and recurs. Decided 2026-10-27 (maintainer), from a
second audit of main against release: the hard fork never merges upstream, so
checking upstream is recurring work. On 2026-10-30 the maintainer moved it into
the vendor sync with moq's other vendored code.

Each run also checks [quinn#2481](https://github.com/quinn-rs/quinn/pull/2481),
the open BBR3 PR carried from
[`55f74c0d`](https://github.com/quinn-rs/quinn/pull/2481/commits/55f74c0dd0738b5ad7d7676c0b2869a547783f83).

Implementation checkpoint 2026-10-31 in
[moq-dev/moq#7458](https://github.com/moq-dev/moq/pull/7458): compared `main`
through [`2c575013`](https://github.com/quinn-rs/quinn/commit/2c5750139fa09aab588bf0f939c83f521d658baf),
`0.11.x` through
[`4b84837d`](https://github.com/quinn-rs/quinn/commit/4b84837df1d25424bc37b0ed990e7b376d40a9c0),
and BBR3 through
[`01b8848d`](https://github.com/quinn-rs/quinn/pull/2481/commits/01b8848dcb321eb17ad4d88dd6f84f28f45a0d6a).
All 13 published advisories predate the fork point. The README records the
ports and why the other changes already hold or do not apply. Batched
`read_datagrams` stays out until the
[planned lock profile](/quest/m2/perf/quic-lock-contention.md) shows a consumer
worth changing the API for. Dependency bumps are skipped.

The implementation ports Apple's IPv4 source override in both UDP send paths,
and BBR3's startup ACK budget, recovery restoration, precautionary probing,
application-limited marker renewal, and saturating probe growth. Four BBR
regressions were reproduced before fixing them. The focused QUIC, socket,
and controller suite passed 808 tests; the benchmark covers recovery too.

Remaining before completing this run:

- Pass the full local `just ci`. Broad Cargo launches currently fail in `mbx`
  with `Argument list too long`, including CI's own Clippy fixture; the
  focused package commands work. Do not treat partial gate results as success.
- Run the Apple source-override regression on macOS at default features and
  with `fast-apple-datapath`. This Linux host cannot reproduce or run that
  platform-specific behavior.
- Obtain the required local other-vendor review and fix or refute every
  finding. The prior batch exhausted Claude review quota; check the actual
  result of the next attempt rather than assuming it is still unavailable.

Public API: none expected; a ported fix that changes one says so. Wire:
QUIC behavior follows upstream's fixes.

## Related

- [moq-quic's API improves on quinn's](/quest/m1/quic-api/README.md) - renames that make each port a hand translation
- [Dependency audit](/quest/m2/audit-deps.md) - covers crates cargo-deny can see
