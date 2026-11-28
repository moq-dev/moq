# [S] A lost identity's Offline survives a coalesced restart and withdrawal

## Goal

When a path's serving identity A is lost, an identity B already present takes over (a pending
`Restart`), and B is then withdrawn before the reader polls, the origin's route stream reports the
path as lost (`Offline`), not finished (`End`). A reader that sees `End` treats the broadcast as
finished when it only went offline. In Rust this is observable on `origin::Consumer::routes()` and
the internal `Follow` cursor; `announced()` and `Following` map both reasons to one absence, so no
public API changes.

## Plan

Noted during [moq-dev/moq#9800](https://github.com/moq-dev/moq/pull/9800), predating it. A loss
alone already reads `Offline`: overwriting a pending `Restart` keeps the retraction reason
(`rs/moq-net/src/model/origin.rs`, `Gone::Offline`). The failing case needs two identities: the
reader observes A, A is lost so B queues a restart, and B is explicitly withdrawn before the reader
polls. The restart drops A's loss reason, so B's `End` is all the reader gets. Reproduce that
sequence in a test, asserting both the identity and the reason the reader receives, then keep the
loss reason through the restart. If it does not reproduce, delete this quest and say so in the PR.
JS has no coalescing yet ([JS announced coalescing](/quest/m3/js-announced-coalesce.md)), so the
JS side of this lands with that quest.

Decided 2026-11-28 (maintainer): ✅ plan it in m3 (rejected: drop until someone hits it).

Public API: none. Wire: none.

## Related

- [JS announced coalescing](/quest/m3/js-announced-coalesce.md) - brings the same overwrite to JS, where this must hold too
