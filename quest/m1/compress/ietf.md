# [M] Compressed tracks over moq-transport

## Goal

Two moq-transport draft-16+ peers that both advertise the compression setup
parameter exchange a compressed track as compressed bytes, with a track
property flagging it and an object extension declaring each object's
inflated size. An IETF peer without the parameter, and draft-14/15, get the
inflated copy as today after [Core](/quest/m1/compress/core.md). moq-relay
accepts a compressed track from a negotiated IETF publisher and forwards it
compressed on lite-07.

## Plan

See the [epic](/quest/m1/compress/README.md) for the decisions.

- A setup parameter is required because receivers discard unknown track
  properties: without it an old peer would read compressed bytes as plain.
- Rewrite `drafts/draft-lcurley-moq-flate.md` as the MoQT extension: the
  codec as today, plus the setup parameter, the track property, and the
  per-object size extension. Pick values outside the ranges the base draft
  reserves; validate with `just drafts check`.
- The property is known to moq-net, so it stays out of the unknown property
  list a relay forwards (`track::Info::properties`), and egress to a peer
  without the parameter drops it.
- Docs inline in `doc/concept/standard.md`.

Tests: a negotiated IETF publisher -> moq-relay -> negotiated IETF and
lite-07 subscribers stays compressed; a non-negotiated IETF subscriber
reads plain. Run `just test interop --all`.

Public API: none beyond Core. Wire: a new setup parameter, track property,
and object extension on draft-16+.

## Required

- [Core](/quest/m1/compress/core.md) - the model stores compressed groups and serves the shared inflated copy
