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

- **Path selection**: when the same namespace arrives over multiple paths, a relay or subscriber has no information with which to prefer one path over another (e.g. the shorter, and usually lower-latency, one). Nor are two paths of equal length interchangeable in practice: one may cross a metered backbone while the other stays inside a datacenter, and one may already be carrying the content while the other would have to fetch it.
- **Broadcast identity**: two advertisements for the same namespace may refer to the same broadcast or to two distinct origins reusing a namespace. With no origin identity a receiver cannot tell them apart, nor deduplicate redundant paths to one broadcast.
- **Routing loops**: relay A advertises a namespace to relay B, which advertises it back to A (directly or through a cycle). Without a way to recognize an advertisement it has already seen, a relay will re-advertise it indefinitely.

This extension solves all three with a single mechanism: an ordered list of **Hop IDs** that records the path an advertisement has taken, starting with the original publisher and with one entry appended per relay.
The first entry identifies the origin (broadcast identity); the list length gives the path length (path selection); a relay finding its own Hop ID already in the list detects a loop.
Hop IDs are unique (see [Hop IDs](#hop-ids)), even across independently operated relays.

A second parameter, **Route Cost**, refines path selection into something a deployment can steer.
Each link declares its price at setup, each advertisement accumulates the prices of the links it crossed, and a receiver prefers the cheapest advertisement rather than merely the shortest.
Pricing every link at the default of 1 reproduces shortest-path routing exactly, so a deployment that does not care about cost need not configure anything.


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

An endpoint that supports the extension MAY additionally declare what this link costs to cross:

~~~
LINK_COST Setup Option {
  Option Key (vi64) = 0x40B56
  Option Value (vi64)
}
~~~

**Option Value**:
The price this connection adds to every advertisement crossing it, in units chosen by the deployment (the same units as [ROUTE_COST](#route_cost-parameter)).
Both endpoints add it to the Route Cost of every advertisement they receive over the connection before forwarding or acting on it, so the link is charged the same from either direction.

An absent option means the default cost of 1, under which the accumulated Route Cost equals the hop count and selection degenerates to shortest-path.
A value of 0 is meaningful and distinct from omitting the option: it makes the link free, which is how a deployment says two relays are siblings in one datacenter.
Larger values price a metered or long-haul link out of contention unless nothing cheaper exists.

Only the client sends it: the price lives in the dialing side's configuration, and the server reads it from the client's SETUP so both ends charge the same link the same amount.
A server MUST NOT send a LINK_COST Setup Option; a client that receives one MUST close the session with a PROTOCOL_VIOLATION.
Like the extension itself, it describes this hop only and a relay MUST NOT forward it.
An endpoint MUST NOT send LINK_COST without also negotiating RELAY_HOPS, since there would be no advertisement field to charge.

The extension applies to a single hop (one MOQT session) and is negotiated independently for each session; a relay MUST NOT assume that because one of its sessions negotiated Relay Hops, another did.

Negotiating this extension on a session also enables the extended NAMESPACE message format defined in [Carrying Parameters on Namespace Advertisements](#carrying-parameters-on-namespace-advertisements), which appends a Parameters field to NAMESPACE so that it, too, can carry HOP_PATH.

A relay that negotiated this extension on a downstream session MUST include the HOP_PATH parameter on every PUBLISH_NAMESPACE and NAMESPACE it sends on that session, and MUST apply the peer's declared Hop ID as described in [Path Selection](#path-selection).
A receiver that negotiated this extension and receives a PUBLISH_NAMESPACE or NAMESPACE without HOP_PATH MUST close the session with a PROTOCOL_VIOLATION.

Message parameters in {{moqt}} have no skip rule at all: an endpoint that receives a Message Parameter it does not know MUST close the session with a PROTOCOL_VIOLATION ({{moqt}} Section 2.5), even when the type is even and its value would be trivially parseable.
An endpoint therefore MUST NOT send HOP_PATH or ROUTE_COST on a session that did not negotiate the extension.
A relay forwarding an advertisement into a non-supporting session strips both (and, for NAMESPACE, the appended Parameters field); the advertisement loses its hop and cost information.

The two parameters are one capability, not two.
An endpoint that sends the RELAY_HOPS Setup Option asserts that it understands HOP_PATH **and** ROUTE_COST, and a peer that negotiated RELAY_HOPS MAY send either without further signalling.
Splitting them would gain nothing and cost correctness: because an unknown parameter is fatal rather than ignorable, a receiver that opted into one but not the other would have to be told which, and every sender would have to track it per session.

That fatality also constrains how this extension may grow.
A future revision MUST NOT add a third parameter under the RELAY_HOPS option, because an endpoint implementing this document would negotiate RELAY_HOPS, receive the unknown parameter, and be required to close the session.
Any new parameter needs its own Setup Option, which the same {{moqt}} section makes safe: unknown *Setup Options* are ignored, so an endpoint that does not recognize the new option simply never receives the parameter.


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
This extension attaches its downstream state (HOP_PATH and ROUTE_COST) to namespace advertisements as Key-Value-Pair parameters (see {{moqt}} Section 2.5).
PUBLISH_NAMESPACE ({{moqt}} Section 10.15) already defines a Parameters field, so both are added to it directly.

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


# ROUTE_COST Parameter
The ROUTE_COST parameter carries the marginal cost of subscribing to the namespace via this advertisement: the price of the transfers that a subscription taken up on it would newly cause.
It is carried alongside HOP_PATH on a namespace advertisement, in the same Parameters field:

~~~
ROUTE_COST Parameter {
  Type (vi64) = 0x40B58
  Value (vi64)
}
~~~

**Value**:
The accumulated cost, in units chosen by the deployment.

ROUTE_COST is OPTIONAL and an absent parameter means 0, so an endpoint that prices nothing sends nothing and a mesh whose peers all omit it selects purely on path length.
Costs still accumulate across such a mesh, because each receiver adds the arriving link's own price (see [LINK_COST](#setup-negotiation)) whether or not the sender declared one.

The original publisher seeds the value with its production cost: 0 for content it is already producing, larger for content it would have to start producing on demand.
A standby transcoder, for example, can advertise every namespace it *could* serve at a cost reflecting the work of actually serving it, and so be chosen only when no live copy exists.


# Relay Behavior
When a relay forwards a namespace advertisement downstream on a session that negotiated this extension, it MUST append its own Hop ID to the HOP_PATH it received.
The relay's own Hop ID is therefore always the last entry of the list it sends.
If the advertisement arrived from an upstream that did not negotiate this extension (and so carried no HOP_PATH), the relay MUST first create a HOP_PATH whose single initial entry is a Hop ID the relay assigns to stand in for that upstream, then append its own Hop ID.
The stand-in MUST be stable for the lifetime of the upstream session and unique per upstream (a random value chosen per session works); advertisements bridged from the same upstream then share an origin, so loop detection, path length, and origin-based deduplication keep working within the supporting region of the mesh.

When a relay receives a namespace advertisement on a session that negotiated this extension, it MUST inspect the HOP_PATH:

- If its own Hop ID already appears in the list, the advertisement has looped back to this relay. The relay MUST discard it: it MUST NOT forward it, and MUST NOT select it as a path to the namespace. Forwarding it would extend the loop, and subscribing through it would route the relay back to itself.
- Otherwise the relay MAY forward it downstream, appending its own Hop ID as described above.

This receiver-side check is the only loop defense this extension requires, and it catches loops of any length.
A relay MAY additionally avoid sending an advertisement back toward a peer it came from, but that is a bandwidth optimization: the advertisement is discarded on arrival either way.

## Accumulating Cost
When a relay receives an advertisement on a session that negotiated this extension, it MUST add that session's link cost (see [Setup Negotiation](#setup-negotiation)) to the ROUTE_COST it received before forwarding or acting on the advertisement.
The addition MUST saturate rather than wrap, so an absurd upstream value ranks last instead of overflowing to best.
Cost therefore accumulates the same way HOP_PATH does, one entry per hop, except that each hop contributes its configured price instead of a fixed 1.

A relay that is actively carrying the namespace (a live subscription exists for at least one of its tracks) SHOULD advertise a ROUTE_COST of 0 instead of the accumulated value.
Its ingress is already paid for, so the marginal cost of one more subscriber is only the links between them, which downstream receivers add themselves.
This is what lets a cluster deduplicate: a receiver that sees both a warm copy at 0 and the original at the full path cost pulls the copy that already exists.

The discount applies only to the advertisement selecting the path the relay actually serves from.
A standby path advertised to a peer whose declared Hop ID filtered out the serving path (see [Path Selection](#path-selection)) keeps its accumulated value, since serving that peer means opening a fresh ingest.
When the relay stops carrying the namespace it SHOULD restore the accumulated value, optionally after a grace period so brief subscriber churn does not flap routing across the mesh.

Two relays that independently begin carrying the same namespace will each see the other's zero-cost advertisement as cheaper than their own source, and switching simultaneously would leave the namespace with no source at all.
An actively-carrying relay SHOULD therefore apply a deterministic tie-break before re-parenting onto a strictly cheaper advertisement from another actively-carrying relay (one advertising a ROUTE_COST of 0 from a HOP_PATH of two or more entries; a single-entry path is the original publisher, which can never adopt a route to its own content), such as comparing a stable hash of the namespace and each endpoint's Hop ID, so that exactly one side moves.
Cheaper advertisements from anything else, e.g. a forwarding relay or a repriced upstream, carry no such hazard and SHOULD be adopted immediately.
The HOP_PATH loop check remains the authority on loop freedom; this tie-break only prevents the transient double-switch.

## Updating an Advertisement
The values this extension carries change over the life of an advertisement: a route fails over and HOP_PATH changes, or a relay starts or stops carrying the namespace and its ROUTE_COST swings to or from 0.
An endpoint updates an advertisement by re-sending it with the new parameters **on the stream that already carries it**: the request stream of the original PUBLISH_NAMESPACE, or the SUBSCRIBE_NAMESPACE response stream the original NAMESPACE arrived on.
A receiver MUST NOT treat that repeat as a duplicate or a protocol violation.

Reusing the stream is what makes the update unambiguous.
In {{moqt}} an advertisement lives for the lifetime of its request stream, so an update sent on a *new* stream would leave two streams claiming one namespace, and closing the superseded one would retract an advertisement the peer had already replaced.
Binding the update to the existing stream keeps one stream owning one advertisement: its parameters are whatever was sent most recently, and closing it retracts whatever is current.
An endpoint MUST NOT open a second stream to advertise a namespace it already advertises on this session.

Replacement is atomic: there is no window in which the namespace is unadvertised, so a receiver MUST NOT tear down subscriptions or forget cached state merely because an update arrived.
What an update means for existing subscriptions depends on the first HOP_PATH entry, exactly as in [Path Selection](#path-selection): unchanged, the content is continuous and a receiver MAY resume in-flight subscriptions on the new route at a group boundary; changed, a different origin has replaced the namespace and existing subscriptions do not carry over.

An update whose only change is ROUTE_COST is the expected case, and is how a relay tells its downstream that it started or stopped carrying the namespace.
An endpoint MAY send one whenever its state changes, without coordinating with the receiver.
Retracting an advertisement is unchanged from {{moqt}}: NAMESPACE_DONE ({{moqt}} Section 10.17) carries no Relay Hops state and is not an update.


# Path Selection
A relay or subscriber that receives advertisements for the same namespace over multiple sessions SHOULD prefer the one with the lowest ROUTE_COST, after adding each arriving link's own cost.
Advertisements that tie on cost SHOULD be broken toward the shorter HOP_PATH (usually the lower-latency path), and those that tie on every advertised property toward the most recently received, so a publisher reconnecting over a new session is not outranked by the session it replaced until the transport declares that one gone.
Selecting on cost with length as the tie-break is what makes the default pricing degrade cleanly: when every link costs 1, cost *is* path length and the two rules collapse into one.

This is advisory: the receiver MAY apply additional local policy (e.g. measured RTT or administrative preference) and is not required to prefer the cheapest path.

Two advertisements for the same namespace whose HOP_PATH begins with the same Hop ID share an origin and therefore carry interchangeable content: a receiver MAY hold them as redundant paths and switch between them, including failing an active subscription over to the surviving path when the serving one ends.
If the first Hop IDs differ, the advertisements come from distinct origins that happen to reuse a namespace, and a receiver MUST NOT treat them as interchangeable; it SHOULD treat the later as a replacement for the earlier rather than serving the earlier until it ends on its own, which would hold the namespace for however long the transport takes to notice a publisher is gone.

A publisher (or relay acting as one) SHOULD advertise, per session, the single best path it knows whose HOP_PATH does not contain the Hop ID the peer declared at setup; when every known path contains it, the publisher SHOULD advertise nothing for that namespace on that session.
Selection is per session: a peer that the serving path flows through receives the best standby path instead of nothing, which is what lets it fail over to that standby if its own copy dies.
If a session's selected path changes, the publisher updates the advertisement in place as described in [Updating an Advertisement](#updating-an-advertisement).

When serving a subscription, a publisher MUST select the source by the same rule it uses for advertisements to that session: a path whose entries avoid the Hop ID the subscriber declared at setup.
If only excluded sources remain, the subscription is unroutable; serving it would hand the subscriber data that already flowed through itself.
Advertisement and dispatch being one selection keeps advertised paths truthful, which is what makes the declared-Hop-ID filter sufficient to prevent subscription cycles of any length: any would-be cycle surfaces the subscriber's own Hop ID inside the candidate path, where the filter removes it.


# Security Considerations
An individual Hop ID reveals nothing about a relay's identity or location beyond what its operator encodes in it; a deployment that considers its configured identifiers sensitive can use random values instead.
A HOP_PATH list does, however, expose the number of hops an advertisement traversed, which can hint at the size and shape of a relay deployment.
A relay that wishes to hide its internal topology MAY coalesce the hops within its own administrative domain into a single Hop ID, or strip HOP_PATH entirely, before forwarding across a trust boundary (for example, to a subscriber outside the operator's own relay cluster).

Because a relay only ever appends to HOP_PATH, it cannot make a competing path appear shorter than it is; the worst a misbehaving relay can do is under-report the upstream portion of its own path to win an advisory tie-break. Since path selection is advisory, the impact is limited to a suboptimal path choice. A receiver MUST NOT make security decisions based on Hop IDs, and SHOULD corroborate path selection with locally measured signals (e.g. RTT) when it matters.

ROUTE_COST offers no such structural protection: it is a single value the sender chooses, so a relay can advertise 0 for content it is not carrying and attract subscriptions it then has to fetch.
The consequence is again a suboptimal path rather than a security failure, and it is self-limiting, since the traffic a relay wins this way is traffic it must pay to serve.
A deployment that spans a trust boundary SHOULD treat a peer's ROUTE_COST as a hint, clamping or ignoring values from peers it does not operate, rather than as an accounting figure.


# IANA Considerations

This document requests the following registrations.
High, distinctive values are requested to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions.

## MOQT Setup Options

This document requests two registrations in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required.

| Value   | Name       | Reference     |
|:--------|:-----------|:--------------|
| 0x40B55 | RELAY_HOPS | This Document |
| 0x40B56 | LINK_COST  | This Document |

## MOQT Message Parameters

This document requests two registrations in the "MOQT Message Parameters" registry ({{moqt}} Section 15.7).
Both are carried in PUBLISH_NAMESPACE and in the extended NAMESPACE message defined by this document (see [Carrying Parameters on Namespace Advertisements](#carrying-parameters-on-namespace-advertisements)).

| Value   | Name        | Carried In                   | Reference     |
|:--------|:------------|:-----------------------------|:--------------|
| 0x40B57 | HOP_PATH    | PUBLISH_NAMESPACE, NAMESPACE | This Document |
| 0x40B58 | ROUTE_COST  | PUBLISH_NAMESPACE, NAMESPACE | This Document |

The Key-Value-Pair encoding of {{moqt}} Section 2.5 makes the parity of each value load-bearing: HOP_PATH and RELAY_HOPS are odd, so their values are length-prefixed byte strings, while ROUTE_COST and LINK_COST are even, so their values are bare varints.


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
