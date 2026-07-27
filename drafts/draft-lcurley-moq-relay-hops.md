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
A namespace subscription MAY carry a single Hop ID to exclude, which a relay uses to suppress advertisements that have already passed through that hop.

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

## Why per-hop, not end-to-end
The Hop ID list is rewritten at every relay: a relay appends its own Hop ID before forwarding an advertisement downstream.
A relay therefore detects a loop by finding its own Hop ID already present in an incoming advertisement, and a subscriber compares path lengths using the list length.
Hop IDs are unique (see [Hop IDs](#hop-ids)), even across independently operated relays.


# Setup Negotiation
The Relay Hops extension is negotiated during the SETUP exchange as defined in {{moqt}} Section 10.3.
An endpoint indicates support by including the following Setup Option:

~~~
RELAY_HOPS Setup Option {
  Option Key (vi64) = 0x40B55
  Option Value Length (vi64) = 0
}
~~~

The extension applies to a single hop (one MOQT session) and is negotiated independently for each session; a relay MUST NOT assume that because one of its sessions negotiated Relay Hops, another did.

Negotiating this extension on a session also enables the extended NAMESPACE message format defined in [Carrying Parameters on Namespace Advertisements](#carrying-parameters-on-namespace-advertisements), which appends a Parameters field to NAMESPACE so that it, too, can carry HOP_PATH.

A relay that negotiated this extension on a downstream session MUST include the HOP_PATH parameter on every PUBLISH_NAMESPACE and NAMESPACE it sends on that session, and MUST honor an EXCLUDE_HOP parameter it receives in SUBSCRIBE_NAMESPACE.
A receiver that negotiated this extension and receives a PUBLISH_NAMESPACE or NAMESPACE without HOP_PATH MUST close the session with a PROTOCOL_VIOLATION.

Message parameters in {{moqt}} have no generic skip rule: a receiver must know a parameter's serialization to parse past it, so an endpoint MUST NOT send HOP_PATH or EXCLUDE_HOP on a session that did not negotiate the extension.
A relay forwarding an advertisement into a non-supporting session therefore strips HOP_PATH (and, for NAMESPACE, the appended Parameters field); the advertisement loses its hop information.


# Hop IDs
A **Hop ID** is a variable-length integer that identifies a single relay (or the original publisher) within the path of an advertisement.

Hop IDs MUST be unique among the endpoints an advertisement can traverse.
Loop detection and origin identification compare Hop IDs for equality, so two endpoints sharing a Hop ID are indistinguishable: advertisements get dropped as false loops, and distinct broadcasts get conflated into one origin.
Deployments often already assign each node a unique identifier; an endpoint SHOULD use such a configured identifier as its Hop ID.
An endpoint with no configured identifier MAY instead draw a full-width random value (up to the 64-bit varint maximum), which is unique with overwhelming probability; a receiver cannot tell how a Hop ID was chosen.
There is no registry and no reserved values: a Hop ID is simply an opaque identifier.

An endpoint SHOULD keep its Hop ID stable for the lifetime of a session (and MAY reuse it across sessions) so that loop detection and path comparison are consistent.

When a relay bridges an advertisement from an upstream peer that did **not** negotiate this extension, the upstream carries no HOP_PATH. The relay MUST synthesize one (see [Relay Behavior](#relay-behavior)) by assigning a random Hop ID to stand in for the unknown upstream, so that loop detection and path length still work within the cooperating region of the mesh.


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

Both endpoints of a session know whether Relay Hops was negotiated, so there is no ambiguity about whether a NAMESPACE message on that session carries the appended Parameters field.
An endpoint MUST NOT append a Parameters field to a NAMESPACE message on a session that did not negotiate Relay Hops, and a receiver on such a session MUST NOT expect one.

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
HOP_PATH always contains at least one entry: the first entry is the Hop ID of the original publisher, even before the advertisement has traversed any relay.

## Relay Behavior
When a relay forwards a namespace advertisement downstream on a session that negotiated this extension, it MUST append its own Hop ID to the HOP_PATH it received.
The relay's own Hop ID is therefore always the last entry of the list it sends.
If the advertisement arrived from an upstream that did not negotiate this extension (and so carried no HOP_PATH), the relay MUST first create a HOP_PATH whose single initial entry is a random Hop ID it assigns to stand in for that unknown upstream, then append its own Hop ID.

When a relay receives a namespace advertisement on a session that negotiated this extension, it MUST inspect the HOP_PATH:

- If its own Hop ID already appears in the list, the advertisement has looped. The relay MUST NOT forward it and SHOULD drop it.
- Otherwise the relay MAY forward it downstream, appending its own Hop ID as described above.

## Path Selection
A relay or subscriber that receives advertisements for the same namespace over multiple sessions MAY use the length of the HOP_PATH list as a tiebreaker, preferring the advertisement with the fewest hops (usually the lowest-latency path).
This is advisory: the receiver MAY apply additional local policy (e.g. measured RTT or administrative preference) and is not required to prefer the shortest path.

Two advertisements for the same namespace whose HOP_PATH begins with the same Hop ID share an origin and therefore refer to the same broadcast; a receiver MAY treat them as redundant paths and keep only the best one.
If the first Hop IDs differ, the advertisements come from distinct origins that happen to reuse a namespace, and a receiver MUST NOT treat them as interchangeable.

A publisher (or relay acting as one) SHOULD advertise only the single best path it currently knows for each namespace.
If the best path changes (for example after a relay failover), the publisher MAY re-advertise the namespace; the new advertisement, carrying an updated HOP_PATH, replaces the prior one per the namespace-advertisement semantics of {{moqt}}.


# EXCLUDE_HOP Parameter
The EXCLUDE_HOP parameter lets a downstream subscriber tell an upstream relay to suppress advertisements that have already passed through a given hop.
A relay in a cluster uses it to prevent the upstream from sending back an advertisement that the downstream originated, the most common source of a two-hop loop.

It is a parameter carried in a SUBSCRIBE_NAMESPACE message ({{moqt}} Section 10.18).
Its value is a single varint:

~~~
EXCLUDE_HOP Parameter {
  Type (vi64) = 0x40B58
  Hop ID (vi64)
}
~~~

**Hop ID**:
The single Hop ID to exclude.
To exclude nothing, a subscriber simply omits the parameter; there is no reserved "exclude nothing" value.

A relay that receives a SUBSCRIBE_NAMESPACE carrying EXCLUDE_HOP MUST NOT send, on that session, any PUBLISH_NAMESPACE whose HOP_PATH contains the excluded Hop ID (including the entry the relay would itself append).
The exclusion is scoped to the namespace subscription it accompanies.

A subscriber MUST NOT send EXCLUDE_HOP on a session that did not negotiate the Relay Hops extension (see [Setup Negotiation](#setup-negotiation)); a receiver that has not negotiated it cannot parse the parameter, let alone honor it.


# Security Considerations
An individual Hop ID reveals nothing about a relay's identity or location beyond what its operator encodes in it; a deployment that considers its configured identifiers sensitive can use random values instead.
A HOP_PATH list does, however, expose the number of hops an advertisement traversed, which can hint at the size and shape of a relay deployment.
A relay that wishes to hide its internal topology MAY coalesce the hops within its own administrative domain into a single Hop ID, or strip HOP_PATH entirely, before forwarding across a trust boundary (for example, to a subscriber outside the operator's own relay cluster).
This is analogous to how BGP confederations hide internal AS topology while preserving loop detection; it is a deployment choice, not a requirement.

Because a relay only ever appends to HOP_PATH, it cannot make a competing path appear shorter than it is; the worst a misbehaving relay can do is under-report the upstream portion of its own path to win an advisory tie-break. Since path selection is advisory, the impact is limited to a suboptimal path choice. A receiver MUST NOT make security decisions based on Hop IDs, and SHOULD corroborate path selection with locally measured signals (e.g. RTT) when it matters.


# IANA Considerations

This document requests the following registrations.
High, distinctive values are requested to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions; they also avoid the greasing pattern (`0x7f * N + 0x9D`).

## MOQT Setup Options

This document requests a registration in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required.

| Value   | Name       | Reference     |
|:--------|:-----------|:--------------|
| 0x40B55 | RELAY_HOPS | This Document |

## MOQT Message Parameters

This document requests registrations in the "MOQT Message Parameters" registry ({{moqt}} Section 15.7).
HOP_PATH is carried in PUBLISH_NAMESPACE and in the extended NAMESPACE message defined by this document (see [Carrying Parameters on Namespace Advertisements](#carrying-parameters-on-namespace-advertisements)); EXCLUDE_HOP is carried in SUBSCRIBE_NAMESPACE.

| Value   | Name        | Carried In                   | Reference     |
|:--------|:------------|:-----------------------------|:--------------|
| 0x40B57 | HOP_PATH    | PUBLISH_NAMESPACE, NAMESPACE | This Document |
| 0x40B58 | EXCLUDE_HOP | SUBSCRIBE_NAMESPACE          | This Document |


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
