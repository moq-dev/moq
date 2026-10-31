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

Last checked: the fork point
[`7616e6b2`](https://github.com/quinn-rs/quinn/commit/7616e6b2782722f3ce4a1b181ef08817d7f545e4)
(2026-10-05), plus the commits the README lists as ported. Upstream `main` was
at [`a2c55b36`](https://github.com/quinn-rs/quinn/commit/a2c55b36817ccaac1c39c6db92de8294f281819f)
(2026-10-30), 18 commits later. Record the newest upstream commit checked in
`rs/moq-quic/README.md`'s `Upstream` section.

First run: four fixes on quinn `main` are not in the ledger yet. Check each
against the fork's code before porting, since a missing ledger entry does not
prove the bug remains:
[`f233510c`](https://github.com/quinn-rs/quinn/commit/f233510c) (honor a
disabled idle timeout when accepting),
[`6c69f9f4`](https://github.com/quinn-rs/quinn/commit/6c69f9f4) (don't
credit a reset stream's unread data twice on stop; the fork rewrote `stop`
under the README's "Stream credit on stop", so it may already hold),
[`623a0369`](https://github.com/quinn-rs/quinn/commit/623a0369) (don't make
specific IPv6 endpoints dual-stack), and
[`a2c55b36`](https://github.com/quinn-rs/quinn/commit/a2c55b36) (IPv4 source
address on Apple, in `moq_sock::udp`). Also judge the new `read_datagrams`
API ([`2697de1e`](https://github.com/quinn-rs/quinn/commit/2697de1e)), a
feature: plan it with `/quest-plan` if moq-tokio's driver would use it.

Public API: none expected; a ported fix that changes one says so. Wire:
QUIC behavior follows upstream's fixes.

## Related

- [moq-quic's API improves on quinn's](/quest/m1/quic-api/README.md) - renames that make each port a hand translation
- [Dependency audit](/quest/m2/audit-deps.md) - covers crates cargo-deny can see
