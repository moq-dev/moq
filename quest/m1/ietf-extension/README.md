# moq-transport passthrough

## Goal

A moq-transport track forwarded moq-transport -> moq-relay -> moq-transport,
including across a relay cluster, arrives with the publisher's subgroups,
object IDs, object properties, END_OF_GROUP status objects, datagram object
IDs, per-subgroup publisher priority, and unknown track properties intact.
Tracks advertising DYNAMIC_GROUPS relay NEW_GROUP_REQUEST back to the
original publisher, including across the cluster, with redundant requests
coalesced.
moxygen's moq-test conformance (sections 1-7) passes through moq-relay in a
nightly CI job.

Boundaries:

- The public API does not change, except one documented field on
  `setup::Extensions`. Every carried field is crate-private in moq-net.
- Plain readers are unchanged: the public Rust and JS APIs, the media layer,
  and a moq-lite session without the extension see subgroup 0 only, frames in
  order with gaps invisible, and no properties.
- Non-goals: full moq-transport parity, accepting PUBLISH (moq-test section 8
  needs it), stitching a FETCH across subgroups or unmarked gaps, JS support,
  and an application API for any of these fields.

## Plan

Decided with the maintainer on 2026-10-13. This revisits the
[#4926](https://github.com/moq-dev/moq/issues/4926) decline for relays only.
That decline's cache-fragmentation concern does not apply here, because
private Object IDs distinguish gaps from frame ordinals, and a FETCH it
cannot answer unambiguously is refused.

- **Carried:** subgroups with their publisher priority, object ID gaps as
  crate-private absolute Object IDs, object properties as a decoded KVP list,
  END_OF_GROUP status objects at their object ID, datagrams past object 0,
  and unknown track properties.
- **Plain readers** see subgroup 0 only. A track with no subgroup 0 reads as
  empty groups. Merging subgroups by object ID can stall on a stream that
  never comes, and it is wrong for SVC and FEC layers. Refusing the track
  would break lite viewers of tracks that merely use subgroups.
- **FETCH** serves a group only if it has one subgroup and every object ID
  jump in it is marked by a Prior Object ID Gap property. Anything else gets
  `NOT_SUPPORTED`.
- **Relay hops** carry the fields over lite-07 through a SETUP extension
  named `ietf`, not a separate ALPN. It is off by default, and moq-relay
  turns it on. A non-relay peer that negotiated it would receive subgroup
  streams and properties it can only discard.
- **Dynamic groups** are supported rather than treated as an opaque track
  property. A private request path carries NEW_GROUP_REQUEST upstream and
  coalesces redundant requests until the largest group advances. Plain
  moq-lite does not advertise this capability.
- **Immutable properties** keep their serialized inner bytes and ingress
  encoding family, including across relay hops. Refuse forwarding between
  the absolute-key/QUIC-varint drafts 14/15, delta-key/QUIC-varint draft 16,
  and delta-key/leading-ones-varint drafts 17+ when immutable properties
  are present. Draft 17 is a separate family because it forbids 7-byte varints.
  Rewriting them would violate immutability; forwarding unchanged
  across those families would change their meaning.
- **Public model carve-out.** These fields stay crate-private on purpose,
  an exception to [a public model](/quest/m1/model-public-surface.md). A
  third-party relay built on the public model drops them, as plain lite
  does.
- **Verification:** Rust integration tests per feature in each child, plus
  the nightly moxygen job owned here.

Work owned by this README, after the children land:

- A nightly CI job runs moxygen's `conformance_test.sh` sections 1-7 with
  `moqtest_server --relay_url` publishing the `moq-test-00` prefix to
  moq-relay. It runs from a pinned moxygen build, at every draft both moxygen
  and moq-relay support, and again across a two-relay cluster.
- The moq-test client fails on any added or removed extension, an exact
  per-subgroup priority (200 or 201) that differs, a missing END_OF_GROUP
  status object, and a FIRST_OBJECT bit that differs. Every child must
  preserve these byte for byte.
- Check routing first. Sections 1-7 send a SUBSCRIBE for a 16-field namespace
  under the announced `moq-test-00` prefix. Whether `origin.dynamic` routes it
  is unverified. If it does not, bring the routing decision to the maintainer
  before working around it.
- FETCH cases are skipped upstream. Section 8 stays out while PUBLISH is
  declined.

## Required

- [Subgroups](/quest/m1/ietf-extension/subgroups.md) - a group holds several subgroups, each forwarded on its own stream with its ID and exact publisher priority; plain readers see subgroup 0
- [Dynamic groups](/quest/m1/ietf-extension/dynamic-groups.md) - NEW_GROUP_REQUEST reaches the original publisher, coalesced across downstream subscribers
- [Track properties](/quest/m1/ietf-extension/track-properties.md) - unknown track properties are forwarded unchanged in SUBSCRIBE_OK, FETCH_OK, and TRACK_STATUS_OK
- [Relay hops](/quest/m1/ietf-extension/lite.md) - the `ietf` SETUP extension carries every crate-private field across a lite-07 hop between relays

## Related

- [#4926](https://github.com/moq-dev/moq/issues/4926) - the declined request for an object-level API; this carries the fields for relays only
- [Plan: a public model](/quest/m1/model-public-surface.md) - these fields are its deliberate exception
