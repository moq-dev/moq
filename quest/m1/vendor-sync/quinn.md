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
the open BBR3 PR moq-congestion carries from
[`55f74c0d`](https://github.com/quinn-rs/quinn/pull/2481/commits/55f74c0dd0738b5ad7d7676c0b2869a547783f83),
for fixes to port, and records the PR commit checked in the README's BBR3
entry. It was force-pushed to `01b8848d` on 2026-10-20.

Last checked: quinn `main` through
[`2697de1e`](https://github.com/quinn-rs/quinn/commit/2697de1e1314cb9f98502b7323acd5bdd836b582)
(2026-10-28) and `0.11.x` through
[`4b84837d`](https://github.com/quinn-rs/quinn/commit/4b84837df1d25424bc37b0ed990e7b376d40a9c0),
recorded in `rs/moq-quic/README.md`'s `Upstream` section
([moq-dev/moq#7287](https://github.com/moq-dev/moq/pull/7287) checked
`f233510c`, `6c69f9f4`, and `623a0369`). Record the newest upstream commit
checked there.

Remaining this run:
[`a2c55b36`](https://github.com/quinn-rs/quinn/commit/a2c55b36817ccaac1c39c6db92de8294f281819f)
(honor IPv4 source addresses on Apple: send `IP_PKTINFO` rather than
`IP_RECVDSTADDR`, in `moq_sock::udp`), the quinn#2481 check above, and judging
the new `read_datagrams` API
([`2697de1e`](https://github.com/quinn-rs/quinn/commit/2697de1e)), a feature:
plan it with `/quest-plan` if moq-tokio's driver would use it.

Learned so far: a fix missing from the ledger is often already held, since the
fork reworked the same code (`stop` under `RESET_STREAM_AT`, the stale-accept
check under `handshake_timeout`, and socket setup in `moq_sock::bind`), so
check the fork's code before porting. A fix to quinn's `Endpoint::client` or
`server` lands in `moq_sock::bind`, which the driver's test helpers bind
through. Most of `0.11.x` is backports of `main`; compare its commits by
subject.

Public API: none expected; a ported fix that changes one says so. Wire:
QUIC behavior follows upstream's fixes.

## Related

- [moq-quic's API improves on quinn's](/quest/m1/quic-api/README.md) - renames that make each port a hand translation
- [Dependency audit](/quest/m2/audit-deps.md) - covers crates cargo-deny can see
