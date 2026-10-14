# [M] The ietf extension carries moq-transport fields between relays

## Goal

Two relays that negotiate the `ietf` SETUP extension carry every
crate-private moq-transport field across a lite-07 hop. A track forwarded
moq-transport publisher -> relay A -> relay B -> moq-transport subscriber
arrives as it would through one relay. A session without the extension is
unchanged.

## Plan

Decided 2026-10-13 (see the [epic](/quest/m1/ietf-extension/README.md)).

- Negotiate with a SETUP extension, like auth and solicit, not a separate
  ALPN. That avoids a Version variant per lite version and leaves the lite-07
  finalize rename alone.
- Add a documented `pub ietf: bool` field on `setup::Extensions`, default
  `false`. moq-relay sets it. Both sides must offer it.
- lite-07 only, while it is the wip version. Frozen versions get no framing
  branches.
- When negotiated, the fields are always present, with no flag bits:
  - The Group message gains the subgroup ID, the optional publisher
    priority, and the END_OF_GROUP status position.
  - Every frame gains a `skip` varint and a length-prefixed properties block,
    2 bytes per frame when empty.
  - Datagrams gain their object ID, properties, and status.
  - Track info gains the unknown track properties.
- Each subgroup travels on its own lite group stream, keyed by (sequence,
  subgroup).
- Write `drafts/draft-lcurley-moq-ietf.md` for the extension and pass
  `just drafts check`. List it with the other project drafts in
  `doc/concept/standard.md`, and note in `doc/bin/relay/` that relays enable
  it.
- js/net does not implement it and never offers it.

Tests: per-feature cases through a two-relay cluster. Cover a cluster peer
without the extension (subgroup 0, no properties), and run
`just test interop --all`.

Public API: `setup::Extensions::ietf`. Wire: a new lite-07 SETUP extension
and the framing above, only when negotiated.

## Required

- [Object fields](/quest/m1/ietf-extension/objects.md) - the per-object fields it frames
- [Subgroups](/quest/m1/ietf-extension/subgroups.md) - the subgroup streams it frames
- [Track properties](/quest/m1/ietf-extension/track-properties.md) - the track properties it frames

## Related

- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - the extension targets the wip version and does not gate the cut
