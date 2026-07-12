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
Each namespace advertisement carries an ordered list of Hop IDs identifying the relays it has traversed, starting with the origin publisher.
This lets a subscriber prefer the shortest of several paths to the same namespace, identify which advertisements refer to the same broadcast (same origin), and lets a relay cluster detect and avoid routing loops.
A namespace subscription MAY carry a single Hop ID to exclude, which a relay uses to suppress advertisements that have already passed through that hop.
For deployments where hops are not equal, the extension also defines an optional weighted **route cost** that generalizes shortest-path selection: each relay adds the cost of the link an advertisement crossed, an origin may attach a standing base cost, and a relay already forwarding a broadcast advertises a cost of zero for it so receivers converge onto paths that already carry the media (cache-aware routing).

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}


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

This extension solves all three with a single mechanism: an ordered list of **Hop IDs** that records the path an advertisement has taken, starting with the origin publisher and with one entry appended per relay.
The first entry identifies the origin (broadcast identity); the list length gives the path length (path selection); a relay finding its own Hop ID already in the list detects a loop.

Path length by itself treats every hop as equal, which real deployments are not. A hop between two relays in the same datacenter is effectively free, while a hop across a metered backbone is expensive; a path through a relay that is *already* forwarding a broadcast costs nothing new upstream, while a fresh path of the same length pays for the whole journey; and an operator may wish to steer traffic toward or away from a particular origin regardless of distance. This extension therefore also defines an optional **route cost** (see [Route Cost](#route-cost)) that refines path selection: it replaces the hop *count* with an additive cost while keeping the very same Hop ID machinery for loop detection and origin identity. A deployment that carries no cost falls back to hop-count selection, so route cost is a strict superset of the shortest-path behavior above.

## Why per-hop, not end-to-end
The Hop ID list is rewritten at every relay: a relay appends its own Hop ID before forwarding an advertisement downstream.
A relay therefore detects a loop by finding its own Hop ID already present in an incoming advertisement, and a subscriber compares path lengths using the list length.
Hop IDs are chosen randomly (see [Hop IDs](#hop-ids)) so they are unique with overwhelming probability without any central coordination, even across independently operated relays.


# Setup Negotiation
The Relay Hops extension is negotiated during the SETUP exchange as defined in {{moqt}} Section 10.3.
An endpoint indicates support by including the following Setup Option:

~~~
RELAY_HOPS Setup Option {
  Option Key (vi64) = 0x40B55
  Option Value Length (vi64)
  [ Link Cost (vi64) ]
}
~~~

**Link Cost** (optional):
Present only when the endpoint also supports the [Route Cost](#route-cost) refinement.
An Option Value Length of 0 negotiates hops-only behavior (no route cost), exactly as before; a non-zero length carries a single varint, the cost this endpoint assigns to the link carried by this session (see [Route Cost](#route-cost)).
Because the value is a property of the connection, the dialing side declares it and both endpoints apply the same value; if both declare one, an endpoint MUST use the value its peer advertised for advertisements it receives on this session, so the two directions agree.
A cost of 1 is the neutral default that reproduces hop-count behavior; 0 marks a free link (for example, two relays in the same datacenter) that routing should prefer.
An endpoint that includes a Link Cost but whose peer's RELAY_HOPS Option Value Length is 0 MUST fall back to hops-only path selection on that session.

The extension applies to a single hop (one MOQT session) and is negotiated independently for each session; a relay MUST NOT assume that because one of its sessions negotiated Relay Hops, another did.

Negotiating this extension on a session also enables the extended NAMESPACE message format defined in [Carrying Parameters on Namespace Advertisements](#carrying-parameters-on-namespace-advertisements), which appends a Parameters field to NAMESPACE so that it, too, can carry HOP_PATH.

A relay that negotiated this extension on a downstream session MUST include the HOP_PATH parameter on every PUBLISH_NAMESPACE and NAMESPACE it sends on that session, and MUST honor an EXCLUDE_HOP parameter it receives in SUBSCRIBE_NAMESPACE.
An endpoint that did not negotiate the extension neither adds these parameters nor, for NAMESPACE, the Parameters field that would carry them.
PUBLISH_NAMESPACE and SUBSCRIBE_NAMESPACE carry a Parameters field in {{moqt}} regardless, and per {{moqt}} an unknown Key-Value-Pair Type is ignored; either way an advertisement forwarded into a non-supporting session loses its hop information gracefully.


# Hop IDs
A **Hop ID** is a variable-length integer that identifies a single relay (or the origin publisher) within the path of an advertisement.

Each relay and each origin publisher chooses its Hop ID **randomly**.
An endpoint SHOULD draw a full-width random value (up to the 64-bit varint maximum) so that the probability of two endpoints choosing the same Hop ID is negligible.
Random assignment means there is no registry, no coordination, and no reserved values: a Hop ID is simply an opaque identifier that is, with overwhelming probability, unique.

An endpoint SHOULD keep its Hop ID stable for the lifetime of a session (and MAY reuse it across sessions) so that loop detection and path comparison are consistent.

When a relay bridges an advertisement from an upstream peer that did **not** negotiate this extension, the upstream carries no HOP_PATH. The relay MUST synthesize one (see [Relay Behavior](#relay-behavior)) by assigning a random Hop ID to stand in for the unknown upstream, so that loop detection and path length still work within the cooperating region of the mesh.


# Carrying Parameters on Namespace Advertisements
This extension attaches its downstream state (HOP_PATH, and optionally ROUTE_COST) to namespace advertisements as Key-Value-Pair parameters (see {{moqt}} Section 2.5).
PUBLISH_NAMESPACE ({{moqt}} Section 10.15) already defines a Parameters field, so these parameters are added to it directly.

The NAMESPACE message ({{moqt}} Section 10.16), which delivers advertisements on a SUBSCRIBE_NAMESPACE response stream, does **not** define a Parameters field in {{moqt}}.
Because a subscriber-driven relay mesh propagates advertisements downstream as NAMESPACE messages, these parameters would otherwise have no way to travel along that path.
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

The Track Namespace Suffix is self-delimiting, so a receiver parses it and then reads the Parameters that follow, bounded by the message Length.
Both endpoints of a session know whether Relay Hops was negotiated, so there is no ambiguity about whether a NAMESPACE message on that session carries the appended Parameters field.
An endpoint MUST NOT append a Parameters field to a NAMESPACE message on a session that did not negotiate Relay Hops, and a receiver on such a session MUST NOT expect one.

This document does not extend NAMESPACE_DONE ({{moqt}} Section 10.17); it carries no Relay Hops state.


# HOP_PATH Parameter
The HOP_PATH parameter carries the ordered list of Hop IDs that an advertisement has traversed, from the origin publisher toward the receiver.
It is a Key-Value-Pair (see {{moqt}} Section 2.5) carried in the Parameters of a namespace advertisement: a PUBLISH_NAMESPACE message ({{moqt}} Section 10.15) or an extended NAMESPACE message (see [Carrying Parameters on Namespace Advertisements](#carrying-parameters-on-namespace-advertisements)).

Because the value is a variable-length list, HOP_PATH uses an odd Type so that it is length-prefixed:

~~~
HOP_PATH Parameter {
  Type (vi64) = 0x40B57
  Length (vi64)
  Hop ID (vi64) ...
}
~~~

**Hop ID**:
One or more Hop IDs, ordered from the origin publisher (first entry) to the relay immediately upstream of the receiver (last entry).
The number of entries is determined by consuming Hop IDs until `Length` bytes have been read; a receiver MUST close the session with a PROTOCOL_VIOLATION if the entries do not exactly fill `Length`, or if the list is empty (`Length` 0).
HOP_PATH always contains at least one entry: the first entry is the Hop ID of the origin publisher, even before the advertisement has traversed any relay.

## Relay Behavior
When a relay forwards a namespace advertisement downstream on a session that negotiated this extension, it MUST append its own Hop ID to the HOP_PATH it received.
The relay's own Hop ID is therefore always the last entry of the list it sends.
If the advertisement arrived from an upstream that did not negotiate this extension (and so carried no HOP_PATH), the relay MUST first create a HOP_PATH whose single initial entry is a random Hop ID it assigns to stand in for that unknown upstream, then append its own Hop ID.

When a relay receives a namespace advertisement on a session that negotiated this extension, it MUST inspect the HOP_PATH:

- If its own Hop ID already appears in the list, the advertisement has looped. The relay MUST NOT forward it and SHOULD drop it.
- Otherwise the relay MAY forward it downstream, appending its own Hop ID as described above.

## Path Selection
A relay or subscriber that receives advertisements for the same namespace over multiple sessions chooses which to route through.
When the [Route Cost](#route-cost) refinement is in use, the receiver SHOULD prefer the advertisement with the lowest total cost; otherwise it MAY use the length of the HOP_PATH list, preferring the advertisement with the fewest hops (usually the lowest-latency path).
In either case selection is advisory: the receiver MAY apply additional local policy (e.g. measured RTT or administrative preference) and is not required to prefer the shortest or cheapest path.

Two advertisements for the same namespace whose HOP_PATH begins with the same Hop ID share an origin and therefore refer to the same broadcast; a receiver MAY treat them as redundant paths and keep only the best one.
If the first Hop IDs differ, the advertisements come from distinct origins that happen to reuse a namespace, and a receiver MUST NOT treat them as interchangeable.

A publisher (or relay acting as one) SHOULD advertise only the single best path it currently knows for each namespace.
If the best path changes — for example after a relay failover — the publisher MAY re-advertise the namespace; the new advertisement, carrying an updated HOP_PATH, replaces the prior one per the namespace-advertisement semantics of {{moqt}}.


# EXCLUDE_HOP Parameter
The EXCLUDE_HOP parameter lets a downstream subscriber tell an upstream relay to suppress advertisements that have already passed through a given hop.
A relay in a cluster uses it to prevent the upstream from sending back an advertisement that the downstream originated, the most common source of a two-hop loop.

It is a Key-Value-Pair carried in the Parameters of a SUBSCRIBE_NAMESPACE message ({{moqt}} Section 10.18).
A single Hop ID is excluded, so EXCLUDE_HOP uses an even Type and its value is a bare varint with no length prefix:

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

A relay that receives EXCLUDE_HOP without having negotiated the Relay Hops extension ignores it as an unknown parameter, which is the safe default (it simply does not perform the exclusion).


# Route Cost
The route cost refines path selection by replacing the hop *count* with an additive weight, so that a receiver can prefer a cheap path over a merely short one.
It is optional: an endpoint negotiates it by including a [Link Cost](#setup-negotiation) in its RELAY_HOPS Setup Option, and carries it as the [ROUTE_COST Parameter](#route_cost-parameter) alongside HOP_PATH.
A deployment that negotiates hops without a Link Cost, or an advertisement that carries no ROUTE_COST, falls back to hop-count [Path Selection](#path-selection).

A **route cost** is a pair of variable-length integers, `(Base Cost, Transit Cost)`, whose saturating sum is the value a receiver compares:

- **Base Cost** is set once by the origin publisher and forwarded by every relay unchanged. It is a standing penalty (or, at 0, a neutral default) attached to a source regardless of how far the advertisement travels — for example, to prefer an already-transcoded rendition over transcoding again downstream.
- **Transit Cost** accumulates along the path. Each relay that forwards an advertisement increases it by the [Link Cost](#setup-negotiation) of the session the advertisement arrived on.

A receiver prefers the advertisement with the lowest `Base Cost + Transit Cost`.
When every Link Cost is 1 and no relay applies the marginal-cost rule below, the Transit Cost equals the HOP_PATH length and cost selection reduces exactly to shortest-path selection.

## Marginal cost
The transit cost a relay advertises depends on what it is currently serving.
A relay that is **actively forwarding** a broadcast (it holds a live subscription to it on behalf of some downstream receiver) MUST advertise a Transit Cost of **zero** for that broadcast when forwarding, because a further downstream receiver pulling through it costs nothing new upstream: the media is already in flight.
This is a marginal-cost signal, not a total-cost one, and it is what lets a mesh converge onto shared paths — cache-aware routing — rather than each receiver independently opening its own expensive path to the origin.
Because the signal changes as subscriptions come and go, the cost of a live advertisement can change during its lifetime; see [Cost Updates](#cost-updates).


# ROUTE_COST Parameter {#route_cost-parameter}
The ROUTE_COST parameter carries the route cost of an advertisement.
It is a Key-Value-Pair (see {{moqt}} Section 2.5) carried in the Parameters of a namespace advertisement: a PUBLISH_NAMESPACE message ({{moqt}} Section 10.15) or an extended NAMESPACE message (see [Carrying Parameters on Namespace Advertisements](#carrying-parameters-on-namespace-advertisements)), the same as HOP_PATH.

Because the value is two integers, ROUTE_COST uses an odd Type so that it is length-prefixed:

~~~
ROUTE_COST Parameter {
  Type (vi64) = 0x40B59
  Length (vi64)
  Base Cost (vi64)
  Transit Cost (vi64)
}
~~~

**Base Cost**:
The origin-set base cost, forwarded unchanged (0 if none).

**Transit Cost**:
The accumulated transit cost as seen by the receiver, i.e. after the sending relay has applied its forwarding rule.

A receiver MUST close the session with a PROTOCOL_VIOLATION if the two integers do not exactly fill `Length`.
An advertisement that omits ROUTE_COST is treated as having a Base Cost of 0 and a Transit Cost equal to its HOP_PATH length.

## Relay Behavior
When a relay forwards a namespace advertisement downstream on a session that negotiated a Link Cost, it MUST set the ROUTE_COST it sends as follows:

- **Base Cost**: copied unchanged from the advertisement it received (0 if none).
- **Transit Cost**: if the relay is currently forwarding an active subscription to this broadcast, `0`; otherwise the received Transit Cost plus the [Link Cost](#setup-negotiation) of the session the advertisement arrived on.

A relay MUST NOT decrease the Base Cost, and MUST NOT decrease the Transit Cost except by the marginal-cost rule (setting it to 0 while actively forwarding).
These bounds ensure that a relay cannot make a competing path look arbitrarily cheap.

## Cost Updates
Unlike the HOP_PATH, which is fixed for the lifetime of an advertisement, the route cost can change while the advertisement is live: the marginal-cost signal flips as the relay starts or stops actively forwarding the broadcast, and an upstream cost change propagates hop by hop.

An endpoint MAY update the cost of a live advertisement by re-advertising the namespace with an updated ROUTE_COST; the re-advertisement replaces the prior one per the namespace-advertisement semantics of {{moqt}}.
Because re-advertising resends the whole advertisement (and, downstream, may interrupt in-flight subscriptions that depend on it), a profile of this extension MAY instead define a lightweight cost-only update that carries just the namespace and a new ROUTE_COST, leaving the rest of the advertisement (including the HOP_PATH) untouched.
Such an update MUST NOT change any field other than the route cost; a receiver applies it by replacing the stored cost and re-evaluating [Path Selection](#path-selection), without treating the advertisement as withdrawn and re-announced.

An endpoint SHOULD damp cost updates so that transient subscription churn does not cause a storm of updates or route flapping:

- A cost **decrease** (a path becoming cheaper, e.g. a relay starting to actively forward) MAY be signaled promptly, since converging onto a cheaper shared path is the goal.
- A cost **increase** (a path becoming more expensive, e.g. the last subscriber leaving so the relay stops actively forwarding) SHOULD be held for a short interval and re-checked before being signaled, so a brief gap between subscribers does not thrash the route.

A receiver that reacts to a cost decrease by switching paths SHOULD introduce a small, deterministic per-node delay before switching, so that two relays whose preferences would mutually flip toward each other do not switch simultaneously; the first to switch suppresses the other's now-looping candidate (via the loop detection above), and the pair converges.
When two advertisements have equal total cost, a receiver breaks the tie by any deterministic, path-stable rule so that every node converges on the same choice; a hash of the namespace and HOP_PATH is RECOMMENDED, since it spreads equal-cost paths across upstreams rather than funneling them onto one.


# Security Considerations
Hop IDs are opaque random integers, so an individual value reveals nothing about a relay's identity or location.
A HOP_PATH list does, however, expose the number of hops an advertisement traversed, which can hint at the size and shape of a relay deployment.
A relay that wishes to hide its internal topology MAY coalesce the hops within its own administrative domain into a single Hop ID, or strip HOP_PATH entirely, before forwarding across a trust boundary (for example, to a subscriber outside the operator's own relay cluster).
This is analogous to how BGP confederations hide internal AS topology while preserving loop detection; it is a deployment choice, not a requirement.

Because a relay only ever appends to HOP_PATH, it cannot make a competing path appear shorter than it is; the worst a misbehaving relay can do is under-report the upstream portion of its own path to win an advisory tie-break. Since path selection is advisory, the impact is limited to a suboptimal path choice. A receiver MUST NOT make security decisions based on Hop IDs, and SHOULD corroborate path selection with locally measured signals (e.g. RTT) when it matters.

The route cost has the same shape of exposure and abuse. The advertised cost can leak the coarse cost structure of a deployment; a relay that wishes to hide it MAY normalize the cost (for example, collapse the transit accumulated within its own administrative domain into a single value) before forwarding across a trust boundary. Because a relay may only increase the Base and Transit Costs — never decrease them, except by the well-defined marginal-cost rule — a misbehaving relay cannot make a competing path look arbitrarily cheap; the worst it can do is under-report its own transit (claim to be actively forwarding when it is not) to win an advisory tie-break, at most steering traffic onto a suboptimal but still loop-free path. A relay MUST bound the rate at which it acts on [Cost Updates](#cost-updates) so that a peer flapping an advertisement's cost cannot force unbounded re-evaluation or a storm of downstream updates.


# IANA Considerations

This document requests the following registrations.
High, distinctive values are requested to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions; they also avoid the greasing pattern (`0x7f * N + 0x9D`).
HOP_PATH and ROUTE_COST each carry a list of integers, so their Types are odd (length-prefixed); EXCLUDE_HOP carries a single Hop ID, so its Type is even (a bare varint). See {{moqt}} Section 2.5.

## MOQT Setup Options

This document requests a registration in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required.

| Value   | Name       | Reference     |
|:--------|:-----------|:--------------|
| 0x40B55 | RELAY_HOPS | This Document |

## MOQT Message Parameters

This document requests registrations in the "MOQT Message Parameters" registry ({{moqt}} Section 15.7).
HOP_PATH and ROUTE_COST are carried in PUBLISH_NAMESPACE and in the extended NAMESPACE message defined by this document (see [Carrying Parameters on Namespace Advertisements](#carrying-parameters-on-namespace-advertisements)); EXCLUDE_HOP is carried in SUBSCRIBE_NAMESPACE.

| Value   | Name        | Carried In                   | Reference     |
|:--------|:------------|:-----------------------------|:--------------|
| 0x40B57 | HOP_PATH    | PUBLISH_NAMESPACE, NAMESPACE | This Document |
| 0x40B58 | EXCLUDE_HOP | SUBSCRIBE_NAMESPACE          | This Document |
| 0x40B59 | ROUTE_COST  | PUBLISH_NAMESPACE, NAMESPACE | This Document |


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
