# [M] JS identity enforcement

## Goal

`@moq/publish` never redefines a track it has served. Once a catalog is
served, a change to a rendition's identity (`codec`, `description`,
`container`, `sampleRate`, `numberOfChannels`) or growth past its
`codedWidth`/`codedHeight` ceiling republishes the broadcast under a new
epoch rather than mutating the track, per the hang draft's
[Track Identity](/drafts/draft-lcurley-moq-hang.md#identity) section. Live
state and rising maxima stay plain catalog updates, and a picture that shrinks
below its fixed ceiling resizes in band with no update at all.

## Plan

- ✅ Decided 2026-10-13 (maintainer, #5459): enforce by republishing under a
  new epoch. Once a catalog is served, an edit that redefines a served
  rendition stops feeding the old catalog track (so it never carries the new
  identity) and `Broadcast` restarts under a new epoch, as it already does
  for a closed rendition track. Refusing the change, or minting a new
  rendition name, were rejected.
- ✅ Decided 2026-10-13 (maintainer, #5459): probe the Opus `description`
  before advertising it. Today the audio encoder adds the encoder-reported
  description on the first encode, which would restart the broadcast on
  every first subscriber. Probe it with a throwaway encoder, as the video
  encoder probes its codec string, using the exact encoder config (kind
  defaults and Opus knobs included, since they move pre-skip).
- ✅ Decided 2026-10-13 (maintainer, #5459): the video ceiling stays the
  per-side maximum of every size encoded, so outgrowing it is the one
  resize that republishes.

Track identities per network broadcast in `CatalogProducer` (keyed by section
and name, kept across a rendition's removal so a returning name is compared
too), recorded from the first serve and cleared when every served track ends.
An absent ceiling may be filled once. Bitrate and framerate decreases are
refused like jitter and delay. The encoders' ceiling and maxima are keyed by
the `Broadcast` component today, so a restart under a new epoch inherits the
old run's limits; key them per epoch with the identities.

What then republishes: video ceiling growth, a codec knob or re-probe that
changes the codec string, an audio capture that changes rate or channel count,
and an audio codec change.

Test a served identity change reaching a new epoch with the old catalog track
ending on the old identity, a returning name, the Opus description present
from the first catalog, and a shrink or maxima rise staying in place.

## Related

- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - the same contract for the Rust publishers
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - a restart is a new epoch rather than a changed track
