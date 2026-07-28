---
title: "MoQ Relay Hops Extension"
abbrev: "moq-relay-hops"
category: info

docname: draft-lcurley-moq-relay-hops-latest
submissiontype: IETF  # also: "independent", "editorial", "IAB", or "IRTF"
number:
date:
v: 3
area: wit
workgroup: moq

author:
 -
    fullname: Luke Curley
    email: kixelated@gmail.com

normative:
  moqt: I-D.ietf-moq-transport

informative:

--- abstract

This document defines a Relay Hops extension for MoQ Transport {{moqt}}.
Each namespace advertisement carries an ordered list of Hop IDs identifying the relays it has traversed, starting with the original publisher.
This lets a subscriber prefer the shortest of several paths to the same namespace, identify which advertisements refer to the same broadcast (same origin), and lets a relay cluster detect and avoid routing loops.
Each endpoint declares its own Hop ID during setup; the peer uses it to suppress advertisements, and to avoid serving subscriptions, whose path has already passed through that endpoint.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}

This document uses **upstream** and **downstream** relative to the flow of a namespace advertisement: an advertisement travels from its original publisher downstream toward subscribers, so on a given session the peer that sends an advertisement is the upstream peer and the peer that receives it is the downstream peer.
{{moqt}} uses these terms relative to the original publisher of the content; in a relay mesh the same pair of relays can carry advertisements in both directions, so here the direction is a property of each advertisement, not of the endpoints.


# Introduction
{{moqt}} is designed to deliver content end-to-end through a mesh of relays.
A namespace advertisement originates at a publisher and propagates downstream through one or more relays toward interested subscribers.
A publisher advertises proactively with PUBLISH_NAMESPACE ({{moqt}} Section 10.15); a subscriber expresses interest with SUBSCRIBE_NAMESPACE ({{moqt}} Section 10.18), and matching advertisements are delivered back on that subscription's response stream as NAMESPACE messages ({{moqt}} Section 10.16).
Both PUBLISH_NAMESPACE and NAMESPACE are namespace advertisements for the purposes of this extension.

In a redundant deployment, relays are interconnected so that the same namespace can reach a given relay over more than one path.
This redundancy is desirable for failover, but it leaves a receiver with no information that {{moqt}} does not address:

- **Path selection**: when the same namespace arrives over multiple paths, a relay or subscriber has no information with which to prefer one path over another (e.g. the shorter, and usually lower-latency, one).
- **Broadcast identity**: two advertisements for the same namespace may refer to the same broadcast or to two distinct origins reusing a namespace. With no origin identity a receiver cannot tell them apart, nor deduplicate redundant paths to one broadcast.
- **Routing loops**: relay A advertises a namespace to relay B, which advertises it back to A (directly or through a cycle). Without a way to recognize an advertisement it has already seen, a relay will re-advertise it indefinitely.

This extension solves all three with a single mechanism: an ordered list of **Hop IDs** that records the path an advertisement has taken, starting with the original publisher and with one entry appended per relay.
The first entry identifies the origin (broadcast identity); the list length gives the path length (path selection); a relay finding its own Hop ID already in the list detects a loop.
Hop IDs are unique (see [Hop IDs](#hop-ids)), even across independently operated relays.


# Setup Negotiation
The Relay Hops extension is negotiated during the SETUP exchange as defined in {{moqt}} Section 10.3.
An endpoint indicates support by including the following Setup Option, whose value declares the endpoint's own Hop ID:

~~~
RELAY_HOPS Setup Option {
  Option Key (vi64) = 0x40B55
  Option Value Length (vi64)
  Hop ID (vi64)
}
~~~

**Hop ID**:
The sender's own Hop ID (see [Hop IDs](#hop-ids)): the identity it appends to HOP_PATH when forwarding advertisements.
An endpoint that never forwards advertisements (a leaf) MAY send an empty value (`Option Value Length` 0), declaring no identity; there is nothing of its own to exclude from a path, and the peer applies no exclusion when selecting advertisements or serving subscriptions for that session.

The extension applies to a single hop (one MOQT session) and is negotiated independently for each session; a relay MUST NOT assume that because one of its sessions negotiated Relay Hops, another did.

Negotiating this extension on a session also enables the extended NAMESPACE message format defined in [Carrying Parameters on Namespace Advertisements](#carrying-parameters-on-namespace-advertisements), which appends a Parameters field to NAMESPACE so that it, too, can carry HOP_PATH.

A relay that negotiated this extension on a downstream session MUST include the HOP_PATH parameter on every PUBLISH_NAMESPACE and NAMESPACE it sends on that session, and MUST apply the peer's declared Hop ID as described in [Path Selection](#path-selection).
A receiver that negotiated this extension and receives a PUBLISH_NAMESPACE or NAMESPACE without HOP_PATH MUST close the session with a PROTOCOL_VIOLATION.

Message parameters in {{moqt}} have no generic skip rule: a receiver must know a parameter's serialization to parse past it, so an endpoint MUST NOT send HOP_PATH on a session that did not negotiate the extension.
A relay forwarding an advertisement into a non-supporting session therefore strips HOP_PATH (and, for NAMESPACE, the appended Parameters field); the advertisement loses its hop information.


# Hop IDs
A **Hop ID** is a variable-length integer that identifies a single relay (or the original publisher) within the path of an advertisement.

Hop IDs MUST be unique among the endpoints an advertisement can traverse.
Loop detection and origin identification compare Hop IDs for equality, so two endpoints sharing a Hop ID are indistinguishable: advertisements get dropped as false loops, and distinct broadcasts get conflated into one origin.
Deployments often already assign each node a unique identifier; an endpoint SHOULD use such a configured identifier as its Hop ID.
An endpoint with no configured identifier MAY instead draw a full-width random value (up to the 64-bit varint maximum), which is unique with overwhelming probability; a receiver cannot tell how a Hop ID was chosen.
There is no registry and no reserved values: a Hop ID is simply an opaque identifier.

An endpoint SHOULD keep its Hop ID stable for the lifetime of a session (and MAY reuse it across sessions) so that loop detection and path comparison are consistent.

Random assignment has one deliberate exception: cooperating redundant publishers MAY share a Hop ID to declare their content interchangeable, so a receiver fails over between their paths (see [Path Selection](#path-selection)).
The default of a fresh random Hop ID per publisher is what makes a restarted publisher look like a new origin rather than a continuation.


# Carrying Parameters on Namespace Advertisements
This extension attaches its downstream state (HOP_PATH) to namespace advertisements as Key-Value-Pair parameters (see {{moqt}} Section 2.5).
PUBLISH_NAMESPACE ({{moqt}} Section 10.15) already defines a Parameters field, so HOP_PATH is added to it directly.

The NAMESPACE message ({{moqt}} Section 10.16), which delivers advertisements on a SUBSCRIBE_NAMESPACE response stream, does **not** define a Parameters field in {{moqt}}.
Because a subscriber-driven relay mesh propagates advertisements downstream as NAMESPACE messages, HOP_PATH would otherwise have no way to travel along that path.
This extension therefore defines an extended NAMESPACE message that appends a Parameters field, used only on a session that negotiated Relay Hops:

~~~
NAMESPACE Message (Relay Hops) {
  Type (vi64) = 0x8,
  Length (16),
  Track Namespace Suffix (..),
  Number of Parameters (vi64),
  Parameters (..) ...
}
~~~

The appended fields use the same encoding as the Parameters field of PUBLISH_NAMESPACE ({{moqt}} Section 10.15):

**Number of Parameters**:
The number of Key-Value-Pair parameters that follow.

**Parameters**:
Zero or more Key-Value-Pairs ({{moqt}} Section 2.5).

An endpoint MUST NOT append a Parameters field to a NAMESPACE message on a session that did not negotiate Relay Hops; both endpoints know whether it was negotiated, so there is no ambiguity about which format applies.

This document does not extend NAMESPACE_DONE ({{moqt}} Section 10.17); it carries no Relay Hops state.


# HOP_PATH Parameter
The HOP_PATH parameter carries the ordered list of Hop IDs that an advertisement has traversed, from the original publisher toward the receiver.
It is a parameter (see {{moqt}} Section 2.5) carried in a namespace advertisement: a PUBLISH_NAMESPACE message ({{moqt}} Section 10.15) or an extended NAMESPACE message (see [Carrying Parameters on Namespace Advertisements](#carrying-parameters-on-namespace-advertisements)).
As with every {{moqt}} message parameter, its serialization is defined here and a receiver only encounters it on a session that negotiated the extension:

~~~
HOP_PATH Parameter {
  Type (vi64) = 0x40B57
  Length (vi64)
  Hop ID (vi64) ...
}
~~~

**Length**:
The length of the Hop ID list in bytes.

**Hop ID**:
One or more Hop IDs, ordered from the original publisher (first entry) to the relay immediately upstream of the receiver (last entry).
A receiver MUST close the session with a PROTOCOL_VIOLATION if the Hop IDs do not exactly fill `Length`, or if the list is empty (`Length` 0).
HOP_PATH always contains at least one entry: the first entry is the Hop ID of the original publisher, even before the advertisement has traversed any relay (or a bridging relay's stand-in for it, see [Relay Behavior](#relay-behavior)).

## Relay Behavior
When a relay forwards a namespace advertisement downstream on a session that negotiated this extension, it MUST append its own Hop ID to the HOP_PATH it received.
The relay's own Hop ID is therefore always the last entry of the list it sends.
If the advertisement arrived from an upstream that did not negotiate this extension (and so carried no HOP_PATH), the relay MUST first create a HOP_PATH whose single initial entry is a Hop ID the relay assigns to stand in for that upstream, then append its own Hop ID.
The stand-in MUST be stable for the lifetime of the upstream session and unique per upstream (a random value chosen per session works); advertisements bridged from the same upstream then share an origin, so loop detection, path length, and origin-based deduplication keep working within the supporting region of the mesh.

When a relay receives a namespace advertisement on a session that negotiated this extension, it MUST inspect the HOP_PATH:

- If its own Hop ID already appears in the list, the advertisement has looped. The relay MUST NOT forward it and SHOULD drop it.
- Otherwise the relay MAY forward it downstream, appending its own Hop ID as described above.

## Path Selection
A relay or subscriber that receives advertisements for the same namespace over multiple sessions MAY use the length of the HOP_PATH list as a tiebreaker, preferring the advertisement with the fewest hops (usually the lowest-latency path).
This is advisory: the receiver MAY apply additional local policy (e.g. measured RTT or administrative preference) and is not required to prefer the shortest path.

Two advertisements for the same namespace whose HOP_PATH begins with the same Hop ID share an origin and therefore carry interchangeable content: a receiver MAY hold them as redundant paths and switch between them, including failing an active subscription over to the surviving path when the serving one ends.
If the first Hop IDs differ, the advertisements come from distinct origins that happen to reuse a namespace, and a receiver MUST NOT treat them as interchangeable; it SHOULD keep serving the earlier one, treating the later as a replacement only once the earlier ends.

A publisher (or relay acting as one) SHOULD advertise, per session, the single best path it knows whose HOP_PATH does not contain the Hop ID the peer declared at setup; when every known path contains it, the publisher SHOULD advertise nothing for that namespace on that session.
Selection is per session: a peer that the serving path flows through receives the best standby path instead of nothing, which is what lets it fail over to that standby if its own copy dies.
If a session's selected path changes, the publisher MAY re-advertise the namespace; the new advertisement, carrying an updated HOP_PATH, replaces the prior one per the namespace-advertisement semantics of {{moqt}}.

When serving a subscription, a publisher MUST select the source by the same rule it uses for advertisements to that session: a path whose entries avoid the Hop ID the subscriber declared at setup.
If only excluded sources remain, the subscription is unroutable; serving it would hand the subscriber data that already flowed through itself.
Advertisement and dispatch being one selection keeps advertised paths truthful, which is what makes the declared-Hop-ID filter sufficient to prevent subscription cycles of any length: any would-be cycle surfaces the subscriber's own Hop ID inside the candidate path, where the filter removes it.


# Security Considerations
An individual Hop ID reveals nothing about a relay's identity or location beyond what its operator encodes in it; a deployment that considers its configured identifiers sensitive can use random values instead.
A HOP_PATH list does, however, expose the number of hops an advertisement traversed, which can hint at the size and shape of a relay deployment.
A relay that wishes to hide its internal topology MAY coalesce the hops within its own administrative domain into a single Hop ID, or strip HOP_PATH entirely, before forwarding across a trust boundary (for example, to a subscriber outside the operator's own relay cluster).

Because a relay only ever appends to HOP_PATH, it cannot make a competing path appear shorter than it is; the worst a misbehaving relay can do is under-report the upstream portion of its own path to win an advisory tie-break. Since path selection is advisory, the impact is limited to a suboptimal path choice. A receiver MUST NOT make security decisions based on Hop IDs, and SHOULD corroborate path selection with locally measured signals (e.g. RTT) when it matters.


# IANA Considerations

This document requests the following registrations.
High, distinctive values are requested to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions.

## MOQT Setup Options

This document requests a registration in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required.

| Value   | Name       | Reference     |
|:--------|:-----------|:--------------|
| 0x40B55 | RELAY_HOPS | This Document |

## MOQT Message Parameters

This document requests a registration in the "MOQT Message Parameters" registry ({{moqt}} Section 15.7).
HOP_PATH is carried in PUBLISH_NAMESPACE and in the extended NAMESPACE message defined by this document (see [Carrying Parameters on Namespace Advertisements](#carrying-parameters-on-namespace-advertisements)).

| Value   | Name        | Carried In                   | Reference     |
|:--------|:------------|:-----------------------------|:--------------|
| 0x40B57 | HOP_PATH    | PUBLISH_NAMESPACE, NAMESPACE | This Document |


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
