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
subgroup other than 0) passes through moq-relay in a nightly CI job.

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
- **Public model carve-out.** These fields stay crate-private on purpose,
  an exception to [a public model](/quest/m1/public-model/README.md). A
  third-party relay built on the public model drops them, as plain lite
  does.
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


### Remaining conformance decision

The original Goal above is unmet. The pinned harness is in
[`test/ietf-conformance`](../../../test/ietf-conformance/README.md), with Moxygen
`c27ae6358df62a1195da479b2e07c50dd8174279` and its matching dependency revisions.
It keeps all selected case arguments and deadlines, excludes only the agreed
nonzero-subgroup cases, and fails on client crashes or publisher reconnects.
The nightly job is a diagnostic checkpoint, not a passing conformance claim.

The direct-origin controls pass 29/29 cases on draft 16 and 28/29 on draft 18.
The draft-18 large-scale case fails even without a relay with
`Group Mismatch: Actual=1 Expected=0`; its provenance must be resolved upstream.
A different single-relay draft-18 case, "End of group markers with single object", also reports `(1, expected 0)`;
its relationship to the direct-control failure is not established.
Every relay cell fails 29/29. Single-relay cells expose the conflicts below;
cluster cells fail before media because their TRACK_INFO query needs upstream
TRACK_STATUS, and the pinned MoQTestPublisher inherits NOT_SUPPORTED. Preserve
that refusal: changing query-only demand into a SUBSCRIBE is not an authorized
fallback here. [Range widening](/quest/m1/compress/ietf-range-widen.md), claimed
by [moq-dev/moq#6465](https://github.com/moq-dev/moq/pull/6465), owns that separate
cold-metadata boundary. The recommended prerequisite is an upstream publisher
that answers metadata queries, plus a validated direct-origin control.

The suite's assumptions conflict with two settled behaviors:

- Its subgroup-zero verifier requires `publisherPriorityForGroup(group)`
  (200 on even groups, 201 on odd groups). This Plan explicitly ranks egress
  by track priority, so a track with priority 200 remains 200.
- It rejects END_OF_TRACK. The relay keeps that status, as decided on
  2026-10-22 in [the runner decision](https://github.com/moq-dev/moq/blob/5f09861634b793056f840408a589f89a8c464fb0/quest/m1/interop-coverage/runner-cases.md).
  Its subgroup-header verifier also applies ordinary payload-group expectations
  to that extra terminal stream. A standalone marker at `(last_group + 1, 0)`
  does not claim END_OF_GROUP for a payload group that never existed.

Retain the hard Goal until a maintainer chooses a compatible verification
contract. Recommended: keep the approved relay behavior and agree an upstream
conformance profile that understands track-level priority and legitimate
END_OF_TRACK, while still checking forwarded object IDs, properties,
END_OF_GROUP and FIRST_OBJECT. That changes the Goal's current requirement that
this selected unmodified suite passes, so it needs approval. The alternative is
to revisit the egress-priority and terminal-status decisions and explicitly
scope their API/wire and scheduling costs. Do not silently alter those decisions,
weaken assertions, or exclude every failing case.

After that decision, rerun both direct-origin controls and all four relay cells,
fix any remaining independently justified forwarding failures at source, and
make the nightly gate pass before deleting this README.

## Related

- [#4926](https://github.com/moq-dev/moq/issues/4926) - the declined request for an object-level API; this carries the fields for relays only
- [A public model](/quest/m1/public-model/README.md) - these fields are its deliberate exception
