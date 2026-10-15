# [XS] Refuse properties on a draft-14 END_OF_TRACK

## Goal

In Rust and JS, a draft-14 END_OF_TRACK object that carries Object
Properties is refused instead of having its properties silently dropped. An END_OF_TRACK without
properties ends the track cleanly, as today.

## Plan

Decided with the maintainer on 2026-10-15: refuse it, don't carry it.
Carrying it would mean storing terminal fields on the track rather than the
group. The project's rule is supported or refused.

Context from [#5494](https://github.com/moq-dev/moq/pull/5494):

- END_OF_TRACK stays a track-level Location.
- Only draft 14 allows properties on it.
- From draft 15, a status object carrying properties is already refused at
  ingest.

Draft 14 permits these properties, so a session `PROTOCOL_VIOLATION` would be
wrong. End the subscription with an error that names it unsupported.

`js/net/src/ietf/object.ts` drops them the same way: it reads past the
extension block and returns the END_OF_TRACK frame without them. Fix both, and
add a regression test in each language that fails without the change.

Public API: none. Wire: a draft-14 publisher that puts properties on
END_OF_TRACK now gets an error instead of a clean end.

## Related

- [moq-transport passthrough](/quest/m1/ietf-extension/README.md) - carries END_OF_GROUP status objects with their properties
