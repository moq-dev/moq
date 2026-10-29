# [S] Compressed tracks in moq-net

## Goal

A publisher sets `compress: true` on a track and moq-net DEFLATEs each group
with the moq-flate format. Relays cache and forward the compressed bytes, and
serve one shared inflated copy, built on demand, to any peer whose protocol
cannot signal compression. One `catalog.json` track replaces `catalog.json`
plus `catalog.json.z`, and the same holds for moq-stats and timelines.

Boundaries:

- Only the original publisher compresses, by opting in. A relay never
  compresses; it only inflates for egress that cannot carry the flag.
- Non-goals: other codecs, per-frame compression opt-out, and compression of
  control messages.

## Plan

Decided with the maintainer on 2026-10-17. This reverses the earlier rule
that compression stays invisible to moq-net
([#1962](https://github.com/moq-dev/moq/pull/1962) removed a per-frame
`compress` flag to follow a spec change; the deleted m2 flate bindings quest kept it
out). The difference now is the group-scoped window: frames compress against
earlier frames of their group, which is where catalogs and stats gain, and
built-in signaling avoids doubling every compressed track's name.

Every interview prompt and the selected answer (✅) are in the planning PR.

- **API:** `compress: bool` on `track::Info` (Rust) and `Info` (JS). One
  codec, no enum; another algorithm would be a breaking change, and none is
  planned.
- **Format:** moq-flate as it is today: raw DEFLATE per group, a sync flush
  per frame with the `00 00 ff ff` marker dropped, a fresh window per group.
- **Uncompressed size on the wire.** Each compressed frame declares its
  inflated size, so `frame::Info.size` stays the size applications see and a
  relay can write an IETF payload length before inflating. Inflated output
  that differs from the declared size is an error. A zip bomb then costs no more than a plain frame
  of the declared size, so there is no separate inflate cap constant: the
  declared size meets the same group and pool limits as a plain frame's
  before any inflate starts, and inflating is incremental, billed as output
  is produced, never preallocated from the declaration.
- **One stored form.** The model stores what arrived, or what the local
  producer compressed. A plain reader (an in-process consumer, or egress to a
  peer that cannot signal compression) reads one inflated copy per group,
  built on first demand, shared across readers, billed to the cache pool, and
  dropped with its group. A thousand old-lite viewers cost one inflate.
- **Signaling:** lite-07 TrackInfo carries the codec id and the frame header the
  size; IETF carries them only
  after both peers advertise a setup parameter, because unknown track
  properties are discarded. Lite-01..06 and IETF peers without the
  parameter get the inflated copy, with no flag. A sealed compression
  declaration is preserved, so delivery is refused when plain fallback would
  remove it.
- **Ranges.** Compressed tracks are not special: every subscription, new or
  resumed, starts at the beginning of a group, and one resuming from another
  route may take the group's start from that route's cached prefix
  (maintainer, on [moq-dev/moq#6465](https://github.com/moq-dev/moq/pull/6465)). Application
  ranges and fetches name whole groups, so a compressed group always
  inflates from a prefix its requester holds.
- **Specs:** draft-lcurley-moq-flate keeps the codec and becomes the IETF
  extension; draft-lcurley-moq-lite specifies the lite-07 field and size and
  references it for the codec.
- **moq-flate and @moq/flate fold into moq-net and js/net.** The snapshot and
  stream track modes go away: a compressed track replaces them. moq-json and
  `@moq/json` drop `Config::compression` (the track flag replaces it, on by
  default for JSON tracks), and
  `moq-mux::binary` is deleted: opaque bytes are a plain track. Decided on
  review of the planning PR.
- **Datagrams** compress alone with a fresh window, never against another
  datagram; lite allows one datagram per group for this reason.
- Compression gates the lite-07 cut, so lite-07 ships with it.

This README owns the end-to-end proof and the guide once every child lands:

- A Rust publisher's compressed `catalog.json` reaches a JS player over
  lite-07 compressed, and a lite-06 and a plain IETF subscriber through
  moq-relay inflated, from one inflate on the relay.
- A short guide under `doc/` on when to set `compress`, what each protocol
  version receives, and the cost on the relay.

## Related

- [Front eviction](/quest/m1/unbounded-groups/front-eviction.md) - a compressed group's evicted prefix must refill before a reader can inflate past it
