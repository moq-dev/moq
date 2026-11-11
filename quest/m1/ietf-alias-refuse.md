# [S] A reused moq-transport alias refuses its track at once

## Goal

When a moq-transport publisher answers a SUBSCRIBE with a track alias that
is already bound, the subscription's readers fail and its track entry is
removed immediately, as every other refusal path does. A peer that never
acknowledges the cancelling stream's FIN can no longer leave readers waiting
on a dead copy or keep a `TrackState` entry registered.

## Plan

Planned 2026-11-10 (maintainer, /quest-complete follow-up of
[moq-dev/moq#8402](https://github.com/moq-dev/moq/pull/8402), from a
CodeRabbit finding declined there because `main` already behaves this way).
Milestone: ✅ m1 (rejected: m2, dropping it), since a misbehaving peer can
hold state unboundedly.

In `rs/moq-net/src/ietf/subscriber.rs`, when `register_alias` fails after
the SUBSCRIBE_OK is accepted, the subscription moves to `OnceStep::Unbound`
and only aborts the track and removes the entry after the peer acknowledges
the FIN. The property-refusal and error branches just above it already
refuse first and then return `OnceStep::Refuse`. Do the same here: reject
the pending readers, remove the subscription and entry, abort the track,
return `OnceStep::Refuse`, and delete `OnceStep::Unbound`. Cover it with a
test whose peer never acknowledges the FIN.

Public API: none. Wire: none.

## Related

- [A reused moq-transport alias delivers a group that beats its SUBSCRIBE_OK](/quest/m1/ietf-alias-reuse.md) - the same alias table, for a reused alias on the same track
