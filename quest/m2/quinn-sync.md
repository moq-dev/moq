# [S] moq-quic has every quinn fix it needs

## Goal

moq-quic, moq-sock's `udp`, and moq-tokio's `quic::driver` carry every
security and correctness fix quinn has landed since the last sync, each
ported with its regression test or recorded as not applying. The fork
ledger in `rs/moq-quic/README.md` says how far upstream was checked.

## Plan

Decided 2026-10-27 (maintainer), from a second audit of main against
release: the hard fork never merges upstream and cargo-deny cannot see a
quinn advisory in in-tree code, so checking upstream is recurring work.

Each run:

- Compare quinn-rs/quinn `main` against the last checked commit, recorded
  in the README's Upstream section (`main` at
  [`2697de1e`](https://github.com/quinn-rs/quinn/commit/2697de1e1314cb9f98502b7323acd5bdd836b582)
  and `0.11.x` at
  [`4b84837d`](https://github.com/quinn-rs/quinn/commit/4b84837df1d25424bc37b0ed990e7b376d40a9c0)
  after the first run), plus quinn's GitHub security advisories.
- Port each fix that applies to code the fork still carries, with its
  regression test, and list it under the README's ported commits. Record
  one that does not apply with its reason, as the README already does.
  Skip dependency bumps and new features.
- Record the newest upstream commit checked in the README, so the next run
  starts there.

Learned in the first run: a fix missing from the ledger is often already
held, since the fork reworked the same code (`stop` under
`RESET_STREAM_AT`, the stale-accept check under `handshake_timeout`, and
socket setup in `moq_sock::bind`), so check the fork's code before porting.
A fix to quinn's `Endpoint::client` or `server` lands in `moq_sock::bind`,
which the driver's test helpers bind through. Most of `0.11.x` is
backports of `main`; compare its commits by subject.

Recurrence: the PR that finishes a run deletes this file and creates the
next run at `/quest/m2/quinn-sync.md`, linked from `/quest/m2/README.md`'s
`Required`. It keeps this Goal and recurrence paragraph, carries forward
what this run learned, and requires [the release cut](/quest/m2/release/cut.md).
The cut removes itself from every `Required` and shifts m2 into m1, so the
next run becomes ready in the new m1, once per release cycle. Only
`/quest-delete` ends the recurrence.

Public API: none expected; a ported fix that changes one says so. Wire:
QUIC behavior follows upstream's fixes.

## Required

- [Release cut](/quest/m2/release/cut.md) - the release in flight ships before this run starts

## Related

- [moq-quic's API improves on quinn's](/quest/m1/quic-api/README.md) - renames that make each port a hand translation
- [Dependency audit](/quest/m2/audit-deps.md) - covers crates cargo-deny can see
