# [M] A relay places a Min Age live join from its upstream's answer

## Goal

A reader that joins at the live edge with a nonzero `Subscriber Min Age`
through a relay starts at the group it would start at reading the publisher
directly, however much of the track the relay's copy holds. In Rust and
`@moq/net`, a copy places that start from its own groups only where they prove
upstream's answer, and otherwise asks upstream and waits for the answer.

## Plan

Found by the 2026-11-20 audit of the subscription rules before the lite-07
cut. A cache only makes things faster (root `AGENTS.md`), but a live join
reaches back only over the groups this hop holds:

- Rust: `joins` (`rs/moq-net/src/model/subscription/window.rs`) through
  `TrackState::reach_back` (`model/track.rs`), whose comment says it is always
  a group this hop holds, so a copy never asks its upstream for the prefix it
  lacks.
- JS: `#reachBack` (`js/net/src/track.ts`).
- The relay already forwards the largest Min Age upstream on SUBSCRIBE and
  SUBSCRIBE_UPDATE, but no reader's start waits on what upstream resolves.

A copy that went live seconds ago serves a 5 s Min Age join from the 1 s it
holds, where the publisher would hand over 5 s.

- Recommended: the copy places the start itself only when it holds every group
  from the live edge back to a group starting at or before the edge minus the
  reader's Min Age, received contiguously since it went live. Otherwise it adds
  the reader's join to its upstream subscription as a range of its own, under
  the reader's Min Age, and starts where that range's SUBSCRIBE_STARTED
  resolves. Groups at or past the live edge keep flowing meanwhile.
- The draft's "The publisher starts from what it holds and never fetches to
  reach further back" stays true of an origin. Add that a relay's start comes
  from its upstream's answer whenever its own holdings can't prove it.
- Share the mechanism with [time-start-asks-upstream](https://github.com/moq-dev/moq/pull/9172)
  (`quest/m1/time-start-asks-upstream.md` on #9172's branch), which asks
  upstream for a time start the same way. Whichever lands first builds it.
- Relative anchors are being removed in another session, which changes how a
  live join is spelled on the wire. Build on whatever spelling has landed.

Reproduce first: a relay whose copy went live 1 s ago, and a reader joining
with a 5 s Min Age against a publisher holding 10 s, in
`rs/moq-net/tests/integration` and `js/net/src/integration.test.ts`.

Public API: none expected. Wire: none; the draft clarifies a relay's start.
