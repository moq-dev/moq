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
Each namespace advertisement carries the ordered list of Hop IDs it has traversed, starting with the original publisher, plus the accumulated cost of that path.
A receiver uses the list to detect routing loops and to identify which advertisements come from the same publisher, and the cost to choose between paths.
Each endpoint declares its own Hop ID during setup, and the peer uses it to avoid advertising or serving a path that already passed through that endpoint.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}

**Upstream** and **downstream** are relative to the flow of an advertisement, not to the endpoints: the peer that sends an advertisement is upstream, the one that receives it is downstream.
The same pair of relays can be upstream of each other for different namespaces.


# Introduction
{{moqt}} is designed to deliver content through a mesh of relays, but is deliberately vague about how that mesh is built, and the base transport does not carry enough information to build one.

Relays that gossip namespaces with PUBLISH_NAMESPACE quickly break down: advertisements loop between relays forever, and when two connections advertise the same namespace, a relay has no basis for deciding which one to route a SUBSCRIBE toward.

This extension adds the HOP_PATH parameter to PUBLISH_NAMESPACE and NAMESPACE.
It lists every node an advertisement has passed through, starting with the original publisher, which is enough to break loops and to compare paths.
Each connection also declares its own Hop ID at SETUP, so loops are avoided even across multiple connections between the same pair of relays.

Not every route is equal: one crossing a metered backbone costs more than one inside a datacenter.
The RELAY_COST Setup Option prices a link, defaulting to 1 so an unpriced mesh simply ranks by hop count.
The ROUTE_COST parameter carries the accumulated price per namespace, and a relay may lower it to advertise that it already has the content cached, steering subscribers toward a warm copy.


# Setup Negotiation

## Relay Hops
The extension is negotiated during the SETUP exchange ({{moqt}} Section 10.3).
An endpoint indicates support with the following Setup Option, whose value is its own Hop ID:

~~~
RELAY_HOPS Setup Option {
  Option Key (vi64) = 0x40B55
  Option Value Length (vi64)
  Hop ID (vi64)
}
~~~

Negotiation is per session; a relay MUST NOT assume that because one of its sessions negotiated Relay Hops, another did.
It also enables the extended NAMESPACE message ({{namespace}}), which is what lets a NAMESPACE carry these parameters at all.

On a session that negotiated the extension, an endpoint MUST include HOP_PATH on every PUBLISH_NAMESPACE and NAMESPACE it sends, and a receiver MUST close the session with a PROTOCOL_VIOLATION if one arrives without it.

## Relay Cost
An endpoint MAY declare what this link costs to cross:

~~~
RELAY_COST Setup Option {
  Option Key (vi64) = 0x40B56
  Option Value (vi64)
}
~~~

Both endpoints add this value to the ROUTE_COST of every advertisement they receive over the connection, so the link is priced the same in both directions.
An absent option means 1, under which the accumulated cost equals the hop count.
0 is meaningful and distinct from absent: it makes the link free, which is how a deployment describes two relays in the same datacenter.


# Hop IDs
A **Hop ID** is a variable-length integer identifying one endpoint within an advertisement's path.

Hop IDs SHOULD be unique among the endpoints an advertisement can traverse.
An endpoint MAY generate one randomly, since collisions across a 64-bit space are unlikely, or use a stable configured identifier that survives restarts.

Loop detection and origin identification compare Hop IDs for equality, so two endpoints sharing a Hop ID are indistinguishable.
Redundant publishers producing interchangeable content MAY share one deliberately, so a receiver treats their paths as failover options for the same content ({{selection}}).

## The Reserved Hop ID 0 {#zero}
**0 means "no identity"** and is reserved.
It is used for an endpoint that did not negotiate this extension, and an endpoint MAY also declare 0 to withhold its identity.

Because any number of endpoints can be 0, it identifies nothing, which constrains all three uses:

- **Loop detection**: 0 in a HOP_PATH is never a loop. A receiver whose own Hop ID is 0 cannot detect loops through itself, and MUST NOT discard an advertisement merely because the path contains 0.
- **Origin identity**: an advertisement whose first entry is 0 has an unknown origin. A receiver MUST NOT treat two such advertisements as interchangeable ({{selection}}).
- **Filtering**: a peer that declared 0 excludes nothing, so the sender applies no filter to that session.

Duplicate *non-zero* Hop IDs in one HOP_PATH are a loop; duplicate zeros are not.
Declaring 0 therefore trades loop detection and failover for anonymity.


# Namespace Advertisements {#namespace}
This extension carries HOP_PATH and ROUTE_COST as Key-Value-Pair parameters ({{moqt}} Section 2.5).
PUBLISH_NAMESPACE ({{moqt}} Section 10.15) already has a Parameters field.

NAMESPACE ({{moqt}} Section 10.16) does not, and a subscriber-driven mesh propagates advertisements as NAMESPACE messages, so this extension defines an extended form used only on a session that negotiated Relay Hops:

~~~
NAMESPACE Message (Relay Hops) {
  Type (vi64) = 0x8,
  Length (16),
  Track Namespace Suffix (..),
  Number of Parameters (vi64),
  Parameters (..) ...
}
~~~

The appended fields are encoded exactly as in PUBLISH_NAMESPACE.
An endpoint MUST NOT append them on a session that did not negotiate the extension.

NAMESPACE_DONE ({{moqt}} Section 10.17) carries no state from this extension and is not extended.

## HOP_PATH Parameter
HOP_PATH is the ordered list of Hop IDs an advertisement has traversed, from the original publisher to the relay immediately upstream of the receiver:

~~~
HOP_PATH Parameter {
  Type (vi64) = 0x40B57
  Length (vi64)
  Hop ID (vi64) ...
}
~~~

The list always has at least one entry, the original publisher, which is 0 if that publisher is unknown ({{zero}}).
A receiver MUST close the session with a PROTOCOL_VIOLATION if the entries do not exactly fill `Length`, if the list is empty, or if a non-zero Hop ID appears twice.

## ROUTE_COST Parameter
ROUTE_COST is the marginal cost of subscribing via this advertisement: the price of the transfers a new subscription would actually cause.

~~~
ROUTE_COST Parameter {
  Type (vi64) = 0x40B58
  Value (vi64)
}
~~~

It is OPTIONAL and absent means 0, so an endpoint that prices nothing sends nothing.
Costs still accumulate across such a mesh, because each receiver adds its own link's price ({{relay-cost}}) regardless.

The original publisher seeds the value with its production cost: 0 for content it is already producing, higher for content it would have to spin up on demand, such as a standby transcoder advertising everything it *could* serve.


# Relay Behavior
When forwarding an advertisement downstream, a relay MUST append its own Hop ID to the HOP_PATH it received, so its own ID is always the last entry.
An advertisement arriving from an upstream that did not negotiate the extension has no HOP_PATH; the relay creates one containing a single 0 for that upstream ({{zero}}), then appends its own.

On receipt, a relay MUST discard an advertisement whose HOP_PATH already contains its own non-zero Hop ID: forwarding it would extend a loop, and subscribing through it would route the relay back to itself.
This receiver-side check catches loops of any length and is the only loop defense required.

## Accumulating Cost
A relay MUST add the session's link cost ({{relay-cost}}) to the ROUTE_COST it received before forwarding or acting on an advertisement.
The addition MUST saturate rather than wrap, so an absurd upstream value ranks last instead of overflowing to best.

A relay actively carrying the namespace (a live subscription exists for at least one of its tracks) SHOULD advertise 0 instead of the accumulated value: its ingress is already paid for, so one more subscriber costs only the links below it.
This is what lets a cluster deduplicate onto a warm copy.
The discount applies only to the advertisement for the path it actually serves from; a standby path keeps its accumulated value, since serving from it means opening a fresh ingest.
When it stops carrying the namespace it SHOULD restore the accumulated value, optionally after a grace period so brief churn does not flap routing.

Two relays that independently begin carrying the same namespace would each see the other's 0 as cheaper than its own source, and both switching at once would leave the namespace with no source.
Before re-parenting onto a 0-cost advertisement from another actively-carrying relay (one whose HOP_PATH has two or more entries), a relay SHOULD apply a deterministic tie-break, such as comparing a hash of the namespace and each Hop ID, so exactly one side moves.
Cheaper advertisements from anything else carry no such hazard and SHOULD be adopted immediately.

## Updating an Advertisement
An endpoint updates an advertisement by re-sending it with new parameters **on the stream that already carries it**: the original PUBLISH_NAMESPACE request stream, or the SUBSCRIBE_NAMESPACE response stream the NAMESPACE arrived on.
A receiver MUST NOT treat the repeat as a duplicate or a protocol violation.

In {{moqt}} an advertisement lives for the lifetime of its stream, so an update on a *new* stream would leave two streams claiming one namespace and let the superseded one retract its replacement.
An endpoint MUST NOT open a second stream for a namespace it already advertises on this session.

Replacement is atomic, so a receiver MUST NOT tear down subscriptions or drop cached state merely because an update arrived.
What it means for existing subscriptions follows the first HOP_PATH entry ({{selection}}): unchanged, the content is continuous and subscriptions MAY resume on the new route at a group boundary; changed, a different publisher has taken over and they do not carry over.

The expected case is a ROUTE_COST-only change, which is how a relay signals that it started or stopped carrying the namespace.


# Path Selection {#selection}
A receiver holding advertisements for the same namespace over several sessions SHOULD prefer the lowest ROUTE_COST, breaking ties toward the shorter HOP_PATH and then toward the most recently received.
This is advisory: a receiver MAY apply local policy such as measured RTT instead.

Two advertisements whose HOP_PATH begins with the same non-zero Hop ID share a publisher and carry interchangeable content, so a receiver MAY hold them as redundant paths and fail an active subscription over to the survivor.
If the first entries differ, or either is 0, they are distinct publishers reusing a namespace: a receiver MUST NOT treat them as interchangeable and SHOULD treat the later as replacing the earlier.

A publisher SHOULD advertise, per session, the best path whose HOP_PATH does not contain the Hop ID that peer declared, and SHOULD advertise nothing when every known path contains it.
Because selection is per session, a peer that the serving path flows through still receives the best standby, which is what lets it fail over if its own copy dies.

When serving a subscription, a publisher MUST select the source by that same rule.
If only excluded sources remain the subscription is unroutable, since serving it would hand the subscriber data that already flowed through itself.
Applying one rule to both advertisement and dispatch keeps advertised paths truthful and prevents subscription cycles of any length.


# Security Considerations
A Hop ID reveals nothing beyond what its operator encodes in it, and a deployment that considers its identifiers sensitive can use random values or declare 0 ({{zero}}).
A HOP_PATH does expose how many hops an advertisement crossed, which hints at the size of a deployment; a relay MAY coalesce its internal hops into one entry, or strip HOP_PATH, before forwarding across a trust boundary.

Because a relay only appends to HOP_PATH, it cannot make a competing path look shorter than it is; the worst it can do is under-report its own upstream portion to win an advisory tie-break.
ROUTE_COST has no such structural protection: it is a single value the sender chooses, so a relay can advertise 0 for content it is not carrying and attract subscriptions it then has to fetch.
Both cost only a suboptimal path choice, and the latter is self-limiting, since the traffic won this way must then be served.

A receiver MUST NOT make security decisions based on Hop IDs, and a deployment spanning a trust boundary SHOULD treat a peer's ROUTE_COST as a hint to clamp or ignore rather than an accounting figure.


# IANA Considerations

This document requests the following registrations.
High, distinctive values are requested to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions.

## MOQT Setup Options

This document requests two registrations in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required.

| Value   | Name       | Reference     |
|:--------|:-----------|:--------------|
| 0x40B55 | RELAY_HOPS | This Document |
| 0x40B56 | RELAY_COST | This Document |

## MOQT Message Parameters

This document requests two registrations in the "MOQT Message Parameters" registry ({{moqt}} Section 15.7).
Both are carried in PUBLISH_NAMESPACE and in the extended NAMESPACE message ({{namespace}}).

| Value   | Name        | Carried In                   | Reference     |
|:--------|:------------|:-----------------------------|:--------------|
| 0x40B57 | HOP_PATH    | PUBLISH_NAMESPACE, NAMESPACE | This Document |
| 0x40B58 | ROUTE_COST  | PUBLISH_NAMESPACE, NAMESPACE | This Document |

The Key-Value-Pair parity is load-bearing: HOP_PATH and RELAY_HOPS are odd, so their values are length-prefixed byte strings, while ROUTE_COST and RELAY_COST are even, so their values are bare varints.


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
