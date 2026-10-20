# [M] The ietf extension records provenance as the ingress version

## Goal

A track crossing a lite-07 hop with the `ietf` SETUP extension carries its
ingress moq-transport version once, instead of an Encoding code on every
Property Block and on TRACK_PROPERTIES. A track published over moq-lite
carries none, and a peer cannot attach moq-transport state to it. Behavior
across the hop is otherwise unchanged from
[moq-dev/moq#6058](https://github.com/moq-dev/moq/pull/6058), which landed
the extension with per-block codes.

## Plan

Decided 2026-10-20 in
[moq-dev/moq#6072](https://github.com/moq-dev/moq/pull/6072).

- In the model, the track's optional ingress `ietf::Version` replaces
  `object::Encoding` on `object::Block` and `TrackProperties`, which already
  derives 0x06's meaning from the draft instead of a flag. One
  version per track suffices because each publisher is its own stream; a
  different ingress is a new instance.
- On the wire, track info carries the ingress version once, or none for a
  track published over moq-lite, which has no IETF provenance and no
  properties to derive rules for. Records and datagrams drop their Encoding
  code. Update `drafts/draft-lcurley-moq-ietf.md` and pass
  `just drafts check`.
- Lite ingress refuses a track with no version that carries any IETF field
  (track, object, or datagram properties, or object IDs), including on a
  later object.
- Immutable properties keep their unchanged inner bytes. Downstream IETF
  egress applies today's single-relay rules from that version: refuse an
  incompatible immutable encoding (drafts 14/15 carry no immutable track
  block, so egress there skips that check), and across draft 19, where 0x06
  becomes SUBGROUP_DELIVERY_TIMEOUT, drop a mutable 0x06 and refuse a sealed
  one.
- Rejected 2026-10-20: a generic crate-private `unknown` KVP block on route,
  track, group, frame, and datagram, tagged by source version, that would
  also tunnel future lite fields through a lite-07 hop. A relay cannot act on
  fields it forwards blindly, and most lite additions (routes, refresh,
  ranges, auth) change relay behavior. With no must-understand marker, only
  relay-ignorable end-to-end hints could tunnel safely.

Tests: 0x06 provenance across the hop: a draft-18 publisher's object and
track properties exit draft 19 with a mutable 0x06 dropped and a sealed one
refused. Both drafts share an encoding family, so only the carried version
distinguishes them. Cover a lite-published track crossing the same hop with
no ingress version, and a malformed peer that sends properties on a
versionless track, at track info and on a later frame and datagram. Run
`just test interop --all`.

Wire: lite-07-wip only, when the extension is negotiated.

## Related

- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - this changes the lite-07 wire, so it lands before the cut
