---
title: "MoQ Cluster Extension"
abbrev: "moq-cluster"
category: info

docname: draft-lcurley-moq-cluster-latest
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

This document defines a clustering extension for MoQ Transport {{moqt}}, used to build a mesh of relays.
Each namespace advertisement carries the list of Hop IDs it has passed through, starting with the original publisher, and the accumulated cost of that path.
A receiver uses the list to detect loops and to tell which advertisements come from the same publisher, and the cost to choose between paths.
Each endpoint declares its own Hop ID at setup, so a peer never advertises or serves it a path that already passed through it.

--- middle

# Note to Readers
This document was written with the assistance of Claude, an AI model by Anthropic.
The author reviewed every revision and is responsible for its content.


# Conventions and Definitions
{::boilerplate bcp14-tagged}

**Upstream** and **downstream** are relative to the flow of an advertisement, not to the endpoints: the peer that sends an advertisement is upstream, the one that receives it is downstream.
The same pair of relays can be upstream of each other for different namespaces.


# Introduction
{{moqt}} is designed to deliver content through a mesh of relays but does not say how to build one, and the base protocol does not carry enough information to do so.
Relays that simply forward PUBLISH_NAMESPACE to each other break down: advertisements loop forever, and a relay that hears one namespace from two peers has no basis for choosing where to send a SUBSCRIBE.

This extension adds two parameters to PUBLISH_NAMESPACE and NAMESPACE.
HOP_PATH lists every endpoint an advertisement has passed through, starting with the original publisher, which breaks loops and lets paths be compared.
ROUTE_COST is the accumulated price of the path: the publisher seeds it, and each hop adds the RELAY_COST its upstream declared at setup, so an unpriced mesh ranks by hop count.
A relay that already carries a namespace advertises a lower cost, steering subscribers toward its warm copy.

Each endpoint also declares its own Hop ID at setup, so a peer can leave it out of every path it advertises or serves to it, even across several connections between the same two relays.
An advertisement is one path, so a relay forwards only the best path it knows per namespace and serves a subscription from one source at a time ({{publishers}}).


# Setup Negotiation

## Hop ID {#hop-id}
The extension is negotiated during SETUP ({{moqt}} Section 10.3).
An endpoint offers it by declaring its own Hop ID:

~~~
HOP_ID Setup Option {
  Option Key (vi64) = 0x40B54
  Hop ID (vi64)
}
~~~

Negotiation is per session; a relay MUST NOT assume that because one session negotiated the extension, another did.
On a session that did, every PUBLISH_NAMESPACE and NAMESPACE MUST carry HOP_PATH, NAMESPACE takes the extended form in {{namespace}}, and a receiver MUST close the session with a PROTOCOL_VIOLATION if either arrives without HOP_PATH.

## Relay Cost {#relay-cost}
An endpoint MAY declare what it charges for sending content:

~~~
RELAY_COST Setup Option {
  Option Key (vi64) = 0x40B56
  Option Value (vi64)
}
~~~

The value prices the sender's own egress, so each endpoint declares its own and the two need not match, as OSPF prices each router's own output interfaces ({{?RFC2328, Section 9}}).
A receiver adds it to the ROUTE_COST of every advertisement that peer forwards ({{accumulating}}).
Absent means 1, so an unpriced mesh ranks by hop count.
0 is distinct from absent: it makes the link free, which is how to describe two relays in the same datacenter.

A declared cost is an assertion, not an instruction: a receiver MAY charge a locally configured value instead, so a peer cannot make itself cheap by saying so.

The cost is one dimensionless integer, as in every deployed routing metric: RIP's hop count ({{?RFC2453, Section 3.5}}), OSPF's interface cost, and IS-IS's default metric, whose delay, expense, and error metrics went unimplemented ({{?RFC5305, Section 3}}), as did OSPF's per-type-of-service metrics ({{?RFC2178, Appendix G.10}}).
A deployment that weighs latency, hop count, and price folds them into the one value.
Like BGP's MULTI_EXIT_DISC ({{?RFC4271, Section 5.1.4}}), the value only means something within the deployment that chose its units, so a trust boundary clamps or replaces it ({{security}}).


# Hop IDs {#hop-ids}
A **Hop ID** is a variable-length integer naming one endpoint in a path.

Hop IDs SHOULD be unique among the endpoints an advertisement can traverse.
An endpoint MAY pick one at random, since collisions in a 64-bit space are unlikely, or use a configured identifier that survives restarts.

Loops and origins are detected by comparing Hop IDs for equality, so two endpoints sharing one are indistinguishable.
Redundant publishers of interchangeable content MAY share one deliberately, so the mesh treats their paths as failover options for the same content ({{selection}}).

## The Reserved Hop ID 0 {#zero}
**0 means "no identity"** and is reserved.
It stands for an endpoint that did not negotiate this extension, and an endpoint MAY declare it to withhold its identity.

Since any number of endpoints can be 0, it identifies nothing:

- **Loop detection**: 0 in a HOP_PATH is never a loop. A receiver whose own Hop ID is 0 cannot detect loops through itself and MUST NOT discard an advertisement merely because the path contains 0.
- **Origin identity**: an advertisement whose first entry is 0 has an unknown publisher. A receiver MUST NOT treat two such advertisements as interchangeable ({{selection}}).
- **Filtering**: a peer that declared 0 gave the receiver nothing to filter that session on. The receiver MAY assign an ID of its own ({{assigned}}).

Duplicate *non-zero* Hop IDs in one HOP_PATH are a loop; duplicate zeros are not.
Declaring 0 trades loop detection and failover for anonymity, except against a receiver that assigns an identity of its own.

## Assigned Identities {#assigned}
A receiver MAY assign a Hop ID to a peer that declared none, whether by declaring 0 or by not negotiating the extension.
It uses that ID wherever it has nothing else to name the peer with: as the first entry of a HOP_PATH it creates for an upstream that sent none, and as what it filters that session on.

The ID is the receiver's own.
An advertisement that arrives with its own HOP_PATH already names the sender there, as 0 if withheld, and this document does not define rewriting that entry.
So an assigned ID covers what the receiver itself attributes to the session; a peer that declares 0 and sends its own HOP_PATH keeps the consequences in {{zero}}.

An assigned ID MUST NOT be shared between peers not known to be the same endpoint.
Sharing one makes their content interchangeable ({{selection}}) and suppresses each one's advertisements to the other, so two unrelated publishers would be merged into one and starve each other of routes.

A peer the receiver authenticated, or dialed and therefore chose, SHOULD get one stable ID, so its reconnects and redundant sessions are recognized as the same content; a fresh ID per connection would make one peer look like several.
An anonymous accepted session cannot be correlated with anything, so it SHOULD get a distinct ID per session: not an identity, but enough to keep routes learned from it from being advertised back to it, which is the loop 0 cannot prevent.

An assigned ID looks like a declared one on the wire, so it identifies the peer to everyone downstream; a peer that declared 0 for anonymity did not ask for that.


# Namespace Advertisements {#namespace}
HOP_PATH and ROUTE_COST are Key-Value-Pair parameters ({{moqt}} Section 2.5).
PUBLISH_NAMESPACE ({{moqt}} Section 10.15) already carries parameters.
NAMESPACE ({{moqt}} Section 10.16) does not, and a subscriber-driven mesh propagates advertisements as NAMESPACE, so on a session that negotiated this extension it takes an extended form:

~~~
NAMESPACE Message (Cluster) {
  Type (vi64) = 0x8,
  Length (16),
  Track Namespace Suffix (..),
  Number of Parameters (vi64),
  Parameters (..) ...
}
~~~

The added fields are encoded exactly as in PUBLISH_NAMESPACE.
An endpoint MUST NOT append them on a session that did not negotiate the extension.
NAMESPACE_DONE ({{moqt}} Section 10.17) is not extended.

## HOP_PATH Parameter {#hop-path}
HOP_PATH is the ordered list of Hop IDs an advertisement has passed through, from the original publisher to the peer sending it:

~~~
HOP_PATH Parameter {
  Type (vi64) = 0x40B57
  Length (vi64)
  Hop ID (vi64) ...
}
~~~

The list always has at least one entry, the original publisher, 0 if unknown ({{zero}}).
A receiver MUST close the session with a PROTOCOL_VIOLATION if the list is empty, if the entries do not exactly fill `Length`, or if a non-zero Hop ID appears twice.

## ROUTE_COST Parameter {#route-cost}
ROUTE_COST is the marginal cost of subscribing through this advertisement: the price of the transfers a new subscription would cause.

~~~
ROUTE_COST Parameter {
  Type (vi64) = 0x40B58
  Value (vi64)
}
~~~

It is OPTIONAL and absent means 0.
Costs still accumulate across a mesh that sends none, because each receiver adds the RELAY_COST of the link it received over ({{accumulating}}).

The original publisher seeds the value with its production cost: 0 for content it already produces, higher for content it would have to start on demand, such as a standby transcoder advertising everything it could serve.


# Relay Behavior
A relay forwarding an advertisement MUST append its own Hop ID to the HOP_PATH it received, so its ID is always the last entry.
An upstream that did not negotiate the extension sends no HOP_PATH; the relay creates one with a single entry for that upstream, 0 ({{zero}}) or an assigned ID ({{assigned}}), then appends its own.

A relay MUST discard an advertisement whose HOP_PATH already contains its own non-zero Hop ID: forwarding it would extend a loop, and subscribing through it would route the relay back to itself.
This check catches loops of any length and is the only loop defense required.
A conforming sender never sends one ({{selection}}), so a receiver MAY close the session with a PROTOCOL_VIOLATION instead; discarding is what keeps the mesh working when one member does not conform.

## Accumulating Cost {#accumulating}
Before forwarding or acting on an advertisement, a relay MUST add the RELAY_COST the sender declared ({{relay-cost}}) to the ROUTE_COST it received.
The addition MUST saturate rather than wrap, so an absurd value ranks last instead of overflowing to best.

A relay actively carrying the namespace (a live subscription exists for at least one of its tracks) SHOULD advertise 0 instead: its ingress is already paid for, so another subscriber costs only the links below it.
This is what lets a cluster converge on a warm copy.
The discount applies only to the path it actually serves from; a standby path keeps its accumulated value, since serving from it means opening a fresh ingest.
When it stops carrying the namespace it SHOULD restore the accumulated value, optionally after a grace period so brief churn does not flap routing.

Two relays that each begin carrying the same namespace would each see the other's 0 as cheaper than its own source, and if both switched at once the namespace would have no source.
Before re-parenting onto a 0-cost advertisement from another actively-carrying relay (one whose HOP_PATH has two or more entries), a relay SHOULD apply a deterministic tie-break, such as comparing a hash of the namespace and each Hop ID, so exactly one side moves.
Equal Hop IDs, including two relays that both declared 0, cannot be ordered, and neither side SHOULD move.
Cheaper advertisements from anything else carry no such hazard and SHOULD be adopted at once.

## Updating an Advertisement {#updating}
An endpoint updates a PUBLISH_NAMESPACE with REQUEST_UPDATE ({{moqt}} Section 9.5) on its request stream, carrying the HOP_PATH or ROUTE_COST that changed.
An omitted parameter keeps its value, so a relay that starts carrying a namespace sends an explicit ROUTE_COST of 0.
The receiver answers REQUEST_OK, or REQUEST_ERROR and closes the stream, which withdraws the advertisement.

NAMESPACE has no REQUEST_UPDATE, so an endpoint updates one by re-sending it with new parameters on the same SUBSCRIBE_NAMESPACE response stream.
A receiver MUST NOT treat the repeat as a duplicate or a protocol violation.

An advertisement lives as long as its stream, so an update on a new stream would leave two streams claiming one namespace.
An endpoint MUST NOT open a second stream for a namespace it already advertises on the session.

An update replaces the old parameters atomically, so a receiver MUST NOT tear down subscriptions or drop cached state because one arrived.
If the first HOP_PATH entry is unchanged the content is continuous and subscriptions MAY resume on the new route at a group boundary, even when that entry is 0: there is one advertisement, and its stream is the continuity.
If the publisher did change, the endpoint MUST withdraw the advertisement (PUBLISH_NAMESPACE_DONE or NAMESPACE_DONE) and advertise again rather than update in place.

The expected update is a ROUTE_COST change, which is how a relay signals that it started or stopped carrying the namespace.


# Path Selection {#selection}
A receiver holding advertisements for one namespace over several sessions SHOULD prefer the lowest ROUTE_COST, breaking ties toward the shorter HOP_PATH and then toward the most recently received.
This is advisory: a receiver MAY apply local policy, such as measured RTT, instead.

Two advertisements whose HOP_PATH begins with the same non-zero Hop ID come from the same publisher and carry interchangeable content: a receiver MAY hold them as redundant paths and fail an active subscription over to the survivor at a group boundary.
If the first entries differ, or either is 0, they are distinct publishers reusing a namespace ({{publishers}}).

An endpoint MUST NOT advertise a path whose HOP_PATH contains the Hop ID the peer declared: the peer could only discard it, and acting on it would form a loop.
Of the paths that remain it SHOULD advertise the best, and advertises nothing when every path contains that Hop ID.
Because selection is per session, a peer that the serving path runs through still receives the best standby, which is what lets it fail over if its own copy dies.

An endpoint MUST select the source for a subscription by the same rule.
If only excluded sources remain the subscription is unroutable, since serving it would hand the subscriber data that already flowed through itself.
One rule for advertisement and dispatch keeps advertised paths truthful and prevents subscription cycles of any length.


# Several Publishers of One Namespace {#publishers}
{{moqt}} lets several publishers advertise one namespace and leaves to the relay how it serves a SUBSCRIBE among them.
Under this extension an advertisement is a path, so a session advertises a namespace at most once, a relay forwards only the best path it knows ({{selection}}), and a subscription is served from one source at a time.

A receiver MAY still hold paths to several publishers of one namespace and choose between them as it sees fit: serve from the cheapest and move to the next when it fails or refuses the request, or try each in cost order until one accepts.
The advertised path and the served source stay the same publisher: a relay that moves to another MUST withdraw its advertisement and advertise the new path ({{updating}}), so the first Hop ID downstream always names the publisher whose Objects flow.
Moving between distinct publishers is a discontinuity: their groups are not one sequence, so a subscriber sees an unrelated Location, and a FETCH that succeeds against one may fail against the other.

Redundant publishers of the same content avoid this by sharing a Hop ID ({{hop-ids}}), which makes their paths interchangeable and lets a subscription fail over at a group boundary.
Publishers that do not share one are treated as reusing a name.


# Security Considerations {#security}
A Hop ID reveals nothing beyond what its operator encodes in it; a deployment that considers its identifiers sensitive can use random values or declare 0 ({{zero}}).
Declaring 0 hides an identity from the mesh but not from the peer, which MAY assign one and forward it onward ({{assigned}}); an endpoint that must stay unlinkable past its first hop cannot get that from this extension.
A HOP_PATH does reveal how many hops an advertisement crossed, which hints at the size of a deployment; a relay MAY collapse its internal hops into one entry, or strip HOP_PATH, before forwarding across a trust boundary.

Because a relay only appends to HOP_PATH, it cannot make a competing path look shorter than it is; the worst it can do is under-report its own upstream portion to win an advisory tie-break.
ROUTE_COST has no such protection: it is a single value the sender chooses, so a relay can advertise 0 for content it is not carrying and attract subscriptions it then has to fetch.
Both cost only a suboptimal path choice, and the latter is self-limiting, since the traffic won this way must then be served.

A receiver MUST NOT make security decisions based on Hop IDs, and a deployment spanning a trust boundary SHOULD treat a peer's ROUTE_COST as a hint to clamp or ignore rather than an accounting figure.


# IANA Considerations

This document requests the following registrations.
High, distinctive values are requested to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions.

## MOQT Setup Options

This document requests two registrations in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required.

| Value   | Name       | Reference     |
|:--------|:-----------|:--------------|
| 0x40B54 | HOP_ID     | This Document |
| 0x40B56 | RELAY_COST | This Document |

## MOQT Message Parameters

This document requests two registrations in the "MOQT Message Parameters" registry ({{moqt}} Section 15.7).
Both are carried in PUBLISH_NAMESPACE, in REQUEST_UPDATE of a PUBLISH_NAMESPACE ({{updating}}), and in the extended NAMESPACE message ({{namespace}}).

| Value   | Name        | Carried In                                   | Reference     |
|:--------|:------------|:---------------------------------------------|:--------------|
| 0x40B57 | HOP_PATH    | PUBLISH_NAMESPACE, REQUEST_UPDATE, NAMESPACE | This Document |
| 0x40B58 | ROUTE_COST  | PUBLISH_NAMESPACE, REQUEST_UPDATE, NAMESPACE | This Document |

The Key-Value-Pair parity is load-bearing: HOP_PATH is odd, so its value is a length-prefixed byte string, while HOP_ID, RELAY_COST, and ROUTE_COST are even, so their values are bare varints.


--- back

# Appendix A: Changelog

## moq-cluster-01
- Renamed the RELAY_HOPS Setup Option to HOP_ID and moved it to the even key 0x40B54, so its value is a bare varint rather than a length-prefixed one.
- A PUBLISH_NAMESPACE is updated with REQUEST_UPDATE on its request stream instead of a repeated PUBLISH_NAMESPACE; HOP_PATH and ROUTE_COST are registered for REQUEST_UPDATE. A NAMESPACE is still re-sent on its stream.
- A session advertises a namespace at most once and a subscription is served from one source at a time. A receiver chooses among several publishers of one namespace; moving between them is a discontinuity unless they share a Hop ID.
- Named the routing protocols whose single per-direction metric RELAY_COST follows.
