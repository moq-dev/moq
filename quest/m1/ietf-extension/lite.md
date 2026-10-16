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
- Target the current wip version. If lite-07 has finalized before this
  lands, use the next wip/version. Published versions get no incompatible
  framing changes.
- When negotiated, the fields are always present, with no flag bits:
  - The Group message gains the explicit FIRST_OBJECT state. The first
    object ID alone cannot recover the FIRST_OBJECT bit. Subgroups other
    than 0 are not carried (see the [epic](/quest/m1/ietf-extension/README.md)).
  - Every frame gains a `skip` varint and a length-prefixed properties block,
    2 bytes per frame when empty.
  - A terminal record after the frames carries the END_OF_GROUP status
    position. It is not known when a live group's opening header is sent;
    frames must flow before the publisher ends the group.
  - Datagrams gain their object ID, properties, and status.
  - Track info gains the unknown track properties.
  - Immutable properties carry their ingress encoding family alongside the
    unchanged inner bytes, so downstream IETF egress can refuse incompatible
    encodings.
- Carry the private NEW_GROUP_REQUEST signal upstream and the dynamic-group
  capability downstream. Coalesce requests as on a direct IETF hop. A peer
  without the extension does not advertise dynamic-group support.
- Write `drafts/draft-lcurley-moq-ietf.md` for the extension and pass
  `just drafts check`. List it with the other project drafts in
  `doc/concept/standard.md`, and note in `doc/bin/relay/` that relays enable
  it.
- js/net does not implement it and never offers it.

Tests: per-feature cases through a two-relay cluster. Cover a cluster peer
without the extension (no properties), and run
`just test interop --all`.
Prove that frames arrive through both relays while the upstream group is
still open, then preserve its terminal status position. Cover FIRST_OBJECT
both set and clear with the same nonzero starting object ID, immutable
encoding provenance, and coalesced dynamic-group requests across the cluster.

Public API: `setup::Extensions::ietf`. Wire: a new lite-07 SETUP extension
and the framing above, only when negotiated.

## Required

- [Track properties](/quest/m1/ietf-extension/track-properties.md) - the track properties it frames
- [Dynamic groups](/quest/m1/ietf-extension/dynamic-groups.md) - the upstream request signal and capability it carries

## Related

- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - the extension targets the wip version and does not gate the cut
