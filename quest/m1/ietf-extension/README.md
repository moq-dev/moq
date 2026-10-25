# [M] moq-transport passthrough

## Goal

A moq-transport track forwarded moq-transport -> moq-relay -> moq-transport,
including across a relay cluster, arrives with the publisher's object IDs,
object properties, END_OF_GROUP status objects, datagram object IDs, and
unknown track properties intact.
Tracks advertising DYNAMIC_GROUPS relay NEW_GROUP_REQUEST back to the
original publisher, with redundant requests coalesced. Across the cluster,
refresh demand carries them on native lite-07.
moxygen's moq-test conformance (sections 1-7, except cases that use a
subgroup other than 0) passes through moq-relay in a nightly CI job under the
relay conformance profile. The profile accepts the track's publisher priority
on every group and an END_OF_TRACK directly after the last object. It still
checks forwarded object IDs, properties, END_OF_GROUP and FIRST_OBJECT.

Boundaries:

- The public API does not change, except one documented field on
  `setup::Extensions`. Every carried field is crate-private in moq-net.
- Plain readers are unchanged: the public Rust and JS APIs, the media layer,
  and a moq-lite session without the extension see frames in order with gaps
  invisible, and no properties.
- Non-goals: full moq-transport parity, subgroups other than 0, accepting
  PUBLISH (moq-test section 8 needs it), stitching a FETCH across unmarked
  gaps, JS support, and an application API for any of these fields.

## Plan

Decided with the maintainer on 2026-10-13. This revisits the
[#4926](https://github.com/moq-dev/moq/issues/4926) decline for relays only.
That decline's cache-fragmentation concern does not apply here, because
private Object IDs distinguish gaps from frame ordinals, and a FETCH it
cannot answer unambiguously is refused.

- **Carried:** object ID gaps as crate-private absolute Object IDs, object
  properties as a decoded KVP list, END_OF_GROUP status objects at their
  object ID, datagrams past object 0, and unknown track properties.
- **Subgroup 0 only.** Decided 2026-10-16 in
  [#5704](https://github.com/moq-dev/moq/pull/5704), which built and then
  reverted a subgroup relay: "don't implement sub-groups. I can't see how
  they could work. Anybody that wants to use an IETF client needs to use
  sub-group 0." Other subgroups are not carried, and their per-stream
  publisher priority with them; egress ranks within a subscription by the
  track's publisher priority.
- **FETCH** serves a group only if every object ID jump in it is marked by a
  Prior Object ID Gap property. Anything else gets `NOT_SUPPORTED`.
- **Relay hops** carry the fields over lite-07 through a SETUP extension
  named `ietf`, not a separate ALPN. It is off by default, and moq-relay
  turns it on. A non-relay peer that negotiated it would receive properties
  and status objects it can only discard.
- **Dynamic groups** are supported rather than treated as an opaque track
  property. The public `Demand::wanted` path (#5930, through relays in
  #6030) carries NEW_GROUP_REQUEST upstream and coalesces redundant requests
  until the largest group advances, so dynamic groups are no longer part of
  the private carve-out below.
- **Immutable properties** keep their serialized inner bytes, and the track
  its ingress moq-transport version, including across relay hops. Refuse forwarding between
  the absolute-key/QUIC-varint drafts 14/15, delta-key/QUIC-varint draft 16,
  and delta-key/leading-ones-varint drafts 17+ when immutable properties
  are present. Draft 17 is a separate family because it forbids 7-byte varints.
  Rewriting them would violate immutability; forwarding unchanged
  across those families would change their meaning.
- **Crate-private on purpose.** These fields never become public API: no
  public moq-net item exists only for moq-transport. A relay that is not
  moq-relay drops them, as plain lite does.
- **Verification:** Rust integration tests per feature in each child, plus
  the nightly moxygen job owned here.

Work owned by this README, after the children land:

- A nightly CI job runs moxygen's `conformance_test.sh` sections 1-7 with
  `moqtest_server --relay_url` publishing the `moq-test-00` prefix to
  moq-relay. It runs from a pinned moxygen build, at every draft both moxygen
  and moq-relay support, and again across a two-relay cluster.
- The moq-test client fails on any added or removed extension, a missing
  END_OF_GROUP status object, and a FIRST_OBJECT bit that differs. Every child must
  preserve these byte for byte.
- Check routing first. Sections 1-7 send a SUBSCRIBE for a 16-field namespace
  under the announced `moq-test-00` prefix. Mock-clock integration tests now
  prove that path on drafts 16 and 18, through one relay and through two relays
  joined by lite-07; the live harness reaches the publisher too.
- FETCH cases are skipped upstream. Section 8 stays out while PUBLISH is
  declined.

### Conformance profile and remaining work

Decided with the maintainer on 2026-10-23: keep the relay's track-level egress
priority and its END_OF_TRACK, and verify under a relay conformance profile
instead of the literal upstream suite. Changing per-group priority transport or
terminal representation to satisfy one verifier would cost scheduling and wire
complexity for no behavioral gain.

The pinned harness is in
[`test/ietf-conformance`](../../../test/ietf-conformance/README.md), with Moxygen
`c27ae6358df62a1195da479b2e07c50dd8174279` and its matching dependency revisions.
It keeps all selected case arguments and deadlines, excludes only the agreed
nonzero-subgroup cases, and fails on client crashes or publisher reconnects.
`nix/moxygen-profile.patch` applies the profile to the client verifier: it also
accepts the track's advertised priority on every group, and one END_OF_TRACK
directly after the last object, whose stream ends no payload group. All other
upstream checks stay, including object IDs, properties, END_OF_GROUP and
FIRST_OBJECT.

Measured on the profile over six runs: the direct-origin controls and the
single-relay cells pass 29/29 on drafts 16 and 18, and the nightly job gates on
them. Two-relay cells failed 29/29 before media, so the harness does not run
them yet. Remaining, in order:

- **A publisher that answers metadata queries.** Cluster cells fail because the
  edge's TRACK_INFO query becomes TRACK_STATUS upstream, and the pinned
  MoQTestPublisher inherits NOT_SUPPORTED. Preserve that refusal: widening
  query-only demand into a SUBSCRIBE is not an authorized fallback.
  No cold request needs that metadata to pick its start, since
  [moq-dev/moq#6465](https://github.com/moq-dev/moq/pull/6465) starts every
  subscription at a group's first frame. Needs a Moxygen revision whose test
  publisher answers TRACK_STATUS; then bump the pins, rebase the profile, and
  add the two-relay cell back to `test/ietf-conformance/run.sh` (it ran in
  [d96126c65a](https://github.com/moq-dev/moq/commit/d96126c65a52027820884cee7cb9741e79fb674b)).
- **The direct draft-18 control.** One checkpoint run failed upstream's "Large
  scale: Many groups and objects" with `Group Mismatch: Actual=1 Expected=0`
  without any relay; it has not recurred in six runs since. The verifier
  expects groups in order although each arrives on its own stream, which its
  own comment calls a spurious failure. Resolve it at the verifier, not with a
  retry.

Then run both direct-origin controls and all four relay cells, fix any
independently justified forwarding failures at source, and keep the nightly
gate passing before deleting this README.

## Related

- [#4926](https://github.com/moq-dev/moq/issues/4926) - the declined request for an object-level API; this carries the fields for relays only
