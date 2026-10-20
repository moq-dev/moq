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
  - Track info gains the unknown track properties: `track::Info::properties`
    (`model::object::TrackProperties`), with its sealed keys.
  - Track info carries the ingress moq-transport version once, or none for a
    track published over moq-lite, which has no IETF provenance and no
    properties to derive rules for. Lite ingress refuses a track with no
    version that carries any IETF field (track, object, or datagram
    properties, or object IDs), including on a later object. Immutable
    properties keep their unchanged inner bytes. Downstream IETF egress
    applies today's single-relay rules from that version: refuse an
    incompatible immutable encoding (drafts 14/15 carry no immutable track
    block, so egress there skips that check), and across draft 19, where
    0x06 becomes SUBGROUP_DELIVERY_TIMEOUT, drop a mutable 0x06 and refuse a
    sealed one.
- Record provenance as the ingress version, decided 2026-10-20. In the
  model, the track's optional ingress `ietf::Version` replaces
  `object::Encoding` and the `delivery_timeout` flags on `Properties` and
  `TrackProperties`; both
  are functions of the ingress draft. One version per track suffices because
  each publisher is its own stream; a different ingress is a new instance.
- Rejected 2026-10-20: a generic crate-private `unknown` KVP block on route,
  track, group, frame, and datagram, tagged by source version, that would
  also tunnel future lite fields through a lite-07 hop. A relay cannot act on
  fields it forwards blindly, and most lite additions (routes, refresh,
  ranges, auth) change relay behavior. With no must-understand marker, only
  relay-ignorable end-to-end hints could tunnel safely. The cost was a
  separate model refactor and more draft surface for a speculative benefit.
  Like this extension, it would have been SETUP-gated, so a receiver opts in
  and plain peers never see stuffed properties.
- Dynamic-group requests are not this extension's job. Native lite-07
  carries them as refresh (TRACK_INFO `Refresh` and SUBSCRIBE_UPDATE
  `Wanted Group`), decided 2026-10-16.
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
encoding provenance, and 0x06 provenance: a draft-18 publisher's object and
track properties exit draft 19 with a mutable 0x06 dropped and a sealed one
refused. Both drafts share an encoding family, so only the carried version
distinguishes them. Cover a lite-published track crossing the same hop with
no ingress version, and a malformed peer that sends properties on a
versionless track, at track info and on a later frame and datagram.

Public API: `setup::Extensions::ietf`. Wire: a new lite-07 SETUP extension
and the framing above, only when negotiated.

## Related

- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - the extension targets the wip version and does not gate the cut
