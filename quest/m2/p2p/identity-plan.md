# [S] Plan P2P identity

## Goal

Condition: the maintainer runs `/quest-plan` on P2P identity and the
[Signaling and policy](/quest/m2/p2p/signal.md) and
[Peer grants](/quest/m2/p2p/peer-grant.md) quests reflect the result. Delete
this quest then.

## Plan

- Parked 2026-10-25 (maintainer): the roster-key question that
  [moq-dev/moq#6310](https://github.com/moq-dev/moq/pull/6310) left open in
  `signal.md` (what names a roster entry and maps an announce to a peer,
  while routing identities are declared and unverified) needs a walkthrough
  before any choice.
- Maintainer direction to explore first: when peers don't trust each other
  and need proof, extend moq-e2ee to sign content (not only encrypt it), so
  proof rides the media rather than a routing identity.

## Related

- [E2EE](/quest/m2/e2ee/README.md) - where signing would live
