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

- Compare quinn-rs/quinn `main` against the last checked commit (the fork
  point [`7616e6b2`](https://github.com/quinn-rs/quinn/commit/7616e6b2782722f3ce4a1b181ef08817d7f545e4)
  for the first run), plus quinn's GitHub security advisories.
- Port each fix that applies to code the fork still carries, with its
  regression test, and list it under the README's ported commits. Record
  one that does not apply with its reason, as the README already does.
  Skip dependency bumps.
- Record the newest upstream commit checked in the README, so the next run
  starts there.

First run: three fixes on quinn `main` are not in the ledger yet. Check
each against the fork's code before porting, since a missing ledger entry
does not prove the bug remains:
[`f233510c`](https://github.com/quinn-rs/quinn/commit/f233510c) (honor a
disabled idle timeout when accepting),
[`6c69f9f4`](https://github.com/quinn-rs/quinn/commit/6c69f9f4) (don't
credit a reset stream's unread data twice on stop; the fork rewrote `stop`
under the README's "Stream credit on stop", so it may already hold), and
[`623a0369`](https://github.com/quinn-rs/quinn/commit/623a0369) (don't make
specific IPv6 endpoints dual-stack).

Recurrence: the PR that finishes a run deletes this file and creates the
next run at `/quest/m2/quinn-sync.md`, linked from `/quest/m2/README.md`'s
`Required`. It keeps this Goal and recurrence paragraph, carries forward
what this run learned, and requires [the release cut](/quest/m2/release/cut.md).
The cut removes itself from every `Required` and shifts m2 into m1, so the
next run becomes ready in the new m1, once per release cycle. Only
`/quest-delete` ends the recurrence.

Public API: none expected; a ported fix that changes one says so. Wire:
QUIC behavior follows upstream's fixes.

## Related

- [moq-quic's API improves on quinn's](/quest/m1/quic-api/README.md) - renames that make each port a hand translation
- [Dependency audit](/quest/m2/audit-deps.md) - covers crates cargo-deny can see
