# [M] Rendition preference

## Goal

A hang video rendition carries an optional `preference` (a signed integer,
absent means 0, higher wins). Automatic selection keeps only the highest
preference among the renditions it can decode, then picks by target and
bitrate inside that tier as today. `<moq-watch>`,
`hang::catalog::Video::ranked`, and WHEP's codec choice honor it, so a viewer
and every single-rendition egress (single-track RTMP play, WHEP, FLV export,
moq-transcode's source pick) only subscribe to a compatibility transcode
(such as H.265 republished as H.264, marked `-1`) when nothing preferred
decodes. A multitrack RTMP client still receives every rendition.

## Plan

Requested by an external consumer (OneTooMany), who publishes an H.265 source
with its own H.264 transcode beside it, so a viewer that can decode H.265
should never pull the transcode.

Decided in planning interviews on 2026-10-01:

- A strict numeric tier, not `fallback: bool`. Two tiers cover the request,
  but a ladder per codec (AV1 > H.265 > H.264) needs three, and today's
  area-then-bitrate sort would pick H.264 over a same-size AV1 rung because
  its bitrate is higher. The tier costs the same to implement as the bool.
- Not a total order (DASH `@qualityRanking`, HLS `SCORE`): the publisher
  would have to rank every rung, and ABR still needs size and bitrate to
  step down a ladder.
- Not a routing-style `cost`: an honest "produced on demand" cost would mark
  moq-transcode's rungs, so the strict rule would stop capable viewers from
  stepping down. The publisher sets `preference` explicitly.
- moq-transcode's rungs inherit their source's `preference`, so a ladder
  adapts within one tier even when the source is not at 0. `rung_entry`
  copies it like `optimize_for_latency`, and the per-snapshot refresh beside
  the `enabled` inheritance keeps it current when the source catalog changes.
- Named `preference`, higher wins, after DASH's `@selectionPriority` (strict
  across per-codec Adaptation Sets, higher preferred). Not `priority`, which
  already means track send priority. Signed, so a new preferred ladder is `1`
  without renumbering existing renditions, and a fallback is `-1`.
- Video only. Optional on the wire, omitted when 0 (as `enabled` is omitted at its default).
  Additive, so older players ignore it.
- Selection order in `js/watch/src/video/source.ts`: drop disabled
  renditions ([enabled flag](/quest/m1/catalog-enabled.md)), then decode
  support, then keep the highest preference among supported renditions, then
  the existing target and bitrate pick within what is left. Preference is
  about decodability only.
- A manual `target.name` still wins over preference. The quality
  picker keeps listing every tier.
- `Video::ranked` sorts by preference (highest first), then by picture and
  bitrate as today. RTMP play, FLV export, and moq-transcode take the first
  rendition they support, so they need no change. Update the
  [JS rendition ranking](/quest/m1/js-ranked.md) Plan if it is still open.
- WHEP needs its own step: `Session::handle_media` (`rs/moq-rtc`) takes the
  peer's first negotiated payload type, then `pick_video` filters `ranked()`
  to that codec, so a peer offering the fallback's codec first would get the
  fallback. Choose across every negotiated video codec so the highest
  supported preference wins.
- Update `rs/hang` `VideoConfig`, `js/hang` `VideoConfigSchema`,
  `drafts/draft-lcurley-moq-hang.md` (next to `enabled`, with a
  source-plus-fallback example), and `doc/concept/hang.md`. No new docs page.
- Tests: a supported source wins over a lower-preference rendition with a
  higher bitrate, and over a bitrate budget that fits only the lower one; an unsupported
  source selects the lower preference; three tiers resolve to the highest
  supported one; a manual `target.name` selects a lower preference; `ranked`
  orders a larger lower-preference rendition after a smaller source; a WHEP
  peer offering the fallback's codec first still gets the source; a
  moq-transcode source at preference 1 under a budget that fits only a rung
  still selects the rung.
- Out of scope: moq-ffi, libmoq, and the bindings until a native player needs
  the field. moq-transcode producing same-size codec fallbacks; the consumer
  publishes its own.

## Related

- [JS rendition ranking](/quest/m1/js-ranked.md) - mirrors `Video::ranked` in `@moq/hang`, which sorts by preference first once this quest lands
- [Audio rendition pick](/quest/m1/audio-ranked.md) - audio ranking, where `preference` could join later
