# [L] SUBSCRIBE_TRACKS and PUBLISH

## Goal

moq-net answers moq-transport SUBSCRIBE_TRACKS (draft-18 on) by sending a
PUBLISH for every track of every broadcast under its namespace prefix, and
accepts an upstream PUBLISH as a track pushed into the model, both through the
SUBSCRIBE_ALL hook. Today both are refused with NOT_SUPPORTED, since moq-net is
pull-only: `ietf/publisher.rs` refuses SUBSCRIBE_TRACKS and `ietf/subscriber.rs`
declines PUBLISH.

## Plan

Decided by the maintainer on 2026-10-17: this belongs to the archive line and
maps 1:1 onto the prefix-scoped
[SUBSCRIBE_ALL](/quest/m1/archive/subscribe-all.md), so IETF clients and
moq-lite share one mechanism; moq-transport adapts to the moq-lite model, not
the reverse. Rejected: one SUBSCRIBE_ALL per announced broadcast under the
prefix, and exact-namespace only.

Guidance:

- TRACK_PROPERTY_FILTER and any MUST that adds cost without a behavioral
  benefit may be refused or skipped; say which in the PR.
- Inbound PUBLISH maps a pushed track onto the receiving broadcast as an
  ordinary track, so a relay serves it exactly as one it subscribed to.
- Cover drafts 18 and later, plus `just test interop --all` and the IETF
  passthrough suite where it exercises these messages.

Test on mock time through a relay: a moq-transport subscriber's
SUBSCRIBE_TRACKS receives a PUBLISH per track, including a later track and a
later broadcast under the prefix; a lite SUBSCRIBE_ALL and an IETF
SUBSCRIBE_TRACKS share one upstream request; an inbound PUBLISH reaches a lite
subscriber.

Public API: none beyond SUBSCRIBE_ALL's hook. Wire: moq-transport support for
two existing messages.

## Required

- [SUBSCRIBE_ALL](/quest/m1/archive/subscribe-all.md) - the model hook this maps onto
