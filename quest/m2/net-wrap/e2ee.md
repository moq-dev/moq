# [M] moq-e2ee implements the moq-net traits

## Goal

moq-e2ee's broadcast, track, and group wrappers implement the
[moq-net traits](/quest/m2/net-wrap/traits.md), so anything generic over them
publishes or consumes an encrypted broadcast unchanged, under semantic track
names.

## Plan

The wrappers merged in #5876 diverge from moq-net, and they adapt to it, not
the reverse (decided 2026-10-20, maintainer):

- `finish(self)` and `abort(self)` without an error become moq-net's
  `finish` and `abort(self, Error)`.
- `track::Producer::create_group(u64)` takes `group::Info`.
- `broadcast::Consumer::subscribe(semantic, ..)`, which returns a
  `kio::Pending`, becomes `track(semantic)` then `.subscribe(..)`.
- `moq_e2ee::Error` converts into `moq_net::Error`, keeping the draft's
  typed codes (such as an authentication failure) distinguishable rather
  than collapsing into `Error::Dropped`.
- Datagrams: `append_datagram`, `insert_datagram`, and `recv_datagram`
  match moq-net's signatures, so the trait read yields only authenticated
  datagrams. A datagram that fails authentication, or a duplicate, is
  dropped there and the track continues. The draft's `authentication`
  event stays on moq-e2ee's own consumer, outside the trait, for callers
  that hold the concrete type. A trait read can't carry it without
  breaking the exact mirror.
- `track::Info.compress` is logical. Ciphertext doesn't compress, so the
  wrapper compresses each group, and each datagram alone with a fresh
  window, with moq-net's
  [track compression](/quest/m1/compress/README.md) before AEAD, creates the
  physical track uncompressed, and inflates after decryption. Sessions drop
  a sealed datagram past the transport's datagram size as they drop an
  oversized plaintext one, so document that the AEAD tag and any compression
  growth count against the caller's payload budget. This keeps
  [rust-publish](/quest/m2/e2ee/rust-publish.md)'s rule that protected
  catalogs compress before encryption, and needs moq-net to expose its group
  codec to the wrapper. `@moq/e2ee` does the same in
  [JS](/quest/m2/net-wrap/js.md).
- The subscriber's wrapper learns the logical flag from authenticated E2EE
  metadata, never from the physical track (decided 2026-10-20, maintainer):
  the physical track's `compress` is always false, so it can't signal it.
  The metadata lives in the E2EE track mapping, and a flag that fails
  authentication fails the track loudly. Not the encrypted catalog: the
  catalog track is itself compressed, so a subscriber would need the flag
  before it could inflate the catalog that carries it. The
  [E2EE epic](/quest/m2/e2ee/README.md) also keeps catalog formats out of
  moq-e2ee, and [migrate](/quest/m1/compress/migrate.md) drops hang's
  `compression` field. The subscriber needs the flag before inflating,
  without changing moq-net's `track(name)`; propose the encoding in the PR.
  Record it in `draft-lcurley-moq-e2ee.md`, replacing the Catalogs section's
  `.z` sibling rule.
- Frames: AEAD authenticates a whole frame, so the wrapper's frame producer
  buffers chunks from `write` and seals at `finish`, and its frame consumer
  yields chunks only after the frame authenticates.
- Demand and preference cross the wrapper: an encrypted track's demand is
  its physical track's, and preference updates reach the physical track.
- Keep the nonce rules the epic specifies: a group producer still owns its
  frame numbering, and a re-created track name is refused, not dropped (see
  [rust-publish](/quest/m2/e2ee/rust-publish.md)).
- Test the wrappers against the questline's pass-through test cases. The
  moq-json round trip through an encrypted track waits for
  [moq-json to take the traits](/quest/m2/net-wrap/json.md).

Public API: the moq-e2ee wrapper signatures change to moq-net's. Wire:
`draft-lcurley-moq-e2ee.md` gains the authenticated logical `compress` flag.

## Required

- [moq-net traits](/quest/m2/net-wrap/traits.md) - the traits this implements
