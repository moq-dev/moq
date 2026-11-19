# [M] moq-e2ee implements the moq-net traits

## Goal

moq-e2ee broadcast, track, and group wrappers implement the
[moq-net traits](/quest/m5/net-wrap/traits.md), so anything generic over them
publishes or consumes an encrypted broadcast unchanged, under semantic track
names.

## Plan

The wrappers merged in #5876 diverge from moq-net, and they adapt to it, not
the reverse (decided 2026-10-20, maintainer):

- Done before this by [the finished format](/quest/m3/e2ee-format.md)
  (decided 2026-11-16, maintainer, /quest-plan of the pre-cut audit):
  `abort` takes an error, `broadcast::Consumer::subscribe` becomes
  `track(semantic)` then `.subscribe(..)`, `track::Consumer` is renamed
  `track::Subscriber`, and `name()` is the semantic name everywhere.
  `track::Producer::create_group` is deleted there rather than changed to
  take `group::Info`: a claim refuses any start at or below the highest
  allocated, so it cannot go back as moq-net's can. The trait's
  `create_group` therefore has no encrypted form; settle here whether the
  trait splits it out or the wrapper answers it with a typed refusal.
- `moq_e2ee::Error` converts into `moq_net::Error`, keeping the draft's
  typed codes (such as an authentication failure) distinguishable rather
  than collapsing into `Error::Dropped`.
- Datagrams: the wrapper's datagram producer and consumer match the
  separate datagram track types, so the trait read
  yields only authenticated datagrams. A datagram that fails authentication, or a duplicate, is
  dropped there and the track continues. The draft's `authentication`
  event stays on moq-e2ee's own consumer, outside the trait, for callers
  that hold the concrete type. A trait read can't carry it without
  breaking the exact mirror.
- Compression (decided 2026-11-16, maintainer, replacing the 2026-10-20
  plan to compress before AEAD and carry an authenticated flag and inflated
  size): the profile carries no compression, so the payload stays
  ciphertext plus tag and the draft gains nothing here. Compressing before
  encrypting leaks length when a frame mixes a secret with input another
  party chooses. An application that wants compression compresses its own
  plaintext. What a trait writer's `track::Info.compression` means on an
  encrypted track is this quest's to settle. Recommended: the wrapper
  creates the physical track uncompressed and reports the logical track as
  uncompressed, so writers work unchanged and a protected catalog ships
  uncompressed; [rust-publish](/quest/m5/e2ee/rust-publish.md) and
  [JS](/quest/m5/net-wrap/js.md) follow.
- Datagram header allowance: a moq-lite datagram carries only Type,
  Subscribe ID, Sequence, and Timestamp, since datagram tracks dropped the
  relayed `ietf` fields and the Inflated Size from it, so
  `MAX_DATAGRAM_HEADER` (28, `rs/moq-e2ee/src/limits.rs` and
  `js/e2ee/src/limits.ts`) covers moq-lite. [moq-dev/moq#7959](https://github.com/moq-dev/moq/pull/7959)
  restores moq-transport datagrams using existing OBJECT_DATAGRAM framing,
  without adding header fields. Re-check the allowance if that framing changes.
- Ranges: every moq-net range starts at a group's first frame
  ([moq-dev/moq#6465](https://github.com/moq-dev/moq/pull/6465)), so the wrapper reads each physical group whole
  and inflates from frame 0.
- Frames: AEAD authenticates a whole frame, so the wrapper's frame producer
  buffers chunks from `write` and seals at `finish`, and its frame consumer
  yields chunks only after the frame authenticates.
- Listing (maintainer, 2026-10-24): the wire listing (moq-lite's Tracks
  stream, IETF SUBSCRIBE_TRACKS) carries physical names unchanged, so
  relays list a protected broadcast like any other. The consumer's `tracks`
  opens each listed physical name and reports its semantic name. A name that does not
  open (relay-forged, another epoch's, malformed) is skipped after one open
  and keeps no state: a relay can inject names into a listing, so ending
  the listing would let it deny discovery. Test a forged and an
  older-epoch name among valid ones. Recovery is crate-private
  `generation::Producer::open` today; move it to the consuming generation,
  which holds the same name keys, so both sides can open.
- Demand and preference cross the wrapper: an encrypted track's demand is
  its physical track's, and preference updates reach the physical track.
- Keep the nonce rules the epic specifies: a group producer still owns its
  frame numbering, and a re-created or re-served track name resumes above
  its high-water marks (decided 2026-10-25, maintainer, replacing the
  refusal; moq-e2ee's claims already resume).
- Test the wrappers against the questline's pass-through test cases. The
  moq-json round trip through an encrypted track waits for
  [moq-json to take the traits](/quest/m5/net-wrap/json.md).

Public API: the moq-e2ee wrappers implement moq-net's traits and their
errors convert into `moq_net::Error`. Wire: none.

## Required

- [moq-net traits](/quest/m5/net-wrap/traits.md) - the traits this implements
- [moq-e2ee publishes the finished format](/quest/m3/e2ee-format.md) - the wrapper names and signatures this starts from
