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
A namespace is routed to one publisher: the mesh carries the best path to it, not one path per publisher ({{single}}).

Not every route is equal: one crossing a metered backbone costs more than one inside a datacenter.
The RELAY_COST Setup Option prices what subscribing from an endpoint costs, defaulting to 1 so an unpriced mesh simply ranks by hop count.
The ROUTE_COST parameter carries the accumulated price per namespace, and a relay may lower it to advertise that it already has the content cached, steering subscribers toward a warm copy.


# Setup Negotiation

## Hop ID {#hop-id}
The extension is negotiated during the SETUP exchange ({{moqt}} Section 10.3).
An endpoint indicates support with the following Setup Option, whose value is its own Hop ID:

~~~
HOP_ID Setup Option {
  Option Key (vi64) = 0x40B54
  Hop ID (vi64)
}
~~~

Negotiation is per session; a relay MUST NOT assume that because one of its sessions negotiated the extension, another did.
It also enables the extended NAMESPACE message ({{namespace}}), which is what lets a NAMESPACE carry these parameters at all.

On a session that negotiated the extension, an endpoint MUST include HOP_PATH on every PUBLISH_NAMESPACE and NAMESPACE it sends, and a receiver MUST close the session with a PROTOCOL_VIOLATION if one arrives without it.

## Relay Cost
An endpoint MAY declare what subscribing from it costs:

~~~
RELAY_COST Setup Option {
  Option Key (vi64) = 0x40B56
  Option Value (vi64)
}
~~~

The option prices one direction, the sender's own egress, so each endpoint declares its own and the two need not match, as OSPF prices each router's own output interfaces ({{?RFC2328, Section 9}}).
A receiver adds the value the sender declared to the ROUTE_COST of every advertisement that sender forwards.
An absent option means 1, under which the accumulated cost equals the hop count.
0 is meaningful and distinct from absent: it makes that direction free, which is how a deployment describes two relays in the same datacenter.

A declared cost is an assertion, not an instruction: a receiver MAY charge a locally configured value instead, so a peer cannot reprice its neighbours by declaring itself cheap.

The cost is one dimensionless integer, as in every widely deployed routing metric: RIP's hop count ({{?RFC2453, Section 3.5}}), OSPF's interface cost, and IS-IS's default metric, whose companion delay, expense, and error metrics went unimplemented ({{?RFC5305, Section 3}}) just as OSPF's per-type-of-service metrics were deleted for lack of use ({{?RFC2178, Appendix G.10}}).
A deployment that weighs several factors, such as latency, hop count, and price, folds them into the one value it declares.
Like BGP's MULTI_EXIT_DISC ({{?RFC4271, Section 5.1.4}}), the value is comparable only within the deployment that chose its units, which is why a trust boundary clamps or replaces it ({{security}}).


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
- **Origin identity**: an advertisement whose first entry is 0 has an unknown origin. A receiver MUST NOT treat two such advertisements as interchangeable ({{selection}}). Updating one advertisement is not two ({{updating}}).
- **Filtering**: a peer that declared 0 declared no identity, so there is nothing on the wire to filter that session on. A receiver MAY assign one ({{assigned}}), which covers what it attributes to that session itself but not an advertisement that arrived carrying its own HOP_PATH.

Duplicate *non-zero* Hop IDs in one HOP_PATH are a loop; duplicate zeros are not.
Declaring 0 therefore trades loop detection and failover for anonymity, except against a receiver that assigns an identity of its own.

## Assigned Identities {#assigned}
A receiver MAY assign a Hop ID of its own to a peer that declared none, whether by declaring 0 or by not negotiating this extension at all.
It uses that ID wherever it would otherwise have nothing to name the peer with: as the entry it creates for an upstream that sent no HOP_PATH, and as what it filters that session on.

The ID is the receiver's own, not the peer's.
An advertisement that arrives carrying its own HOP_PATH names the sender there, as 0 if the sender withheld it, and this document does not define rewriting that entry.
So an assigned ID governs the advertisements a receiver attributed itself, and a peer that both declares 0 and sends its own HOP_PATH keeps the consequences in {{zero}}.

An assigned ID MUST NOT be shared between peers not known to be the same endpoint.
Sharing one makes their content interchangeable ({{selection}}) and suppresses each one's advertisements to the other, so two unrelated publishers would be spliced into one and starve each other of routes.

How an endpoint scopes the ID follows from what it can establish about the peer.
One it authenticated, or one it dialed and therefore chose, SHOULD get a single stable ID, which additionally lets reconnects and redundant sessions be recognized as the same content ({{selection}}); assigning per connection there would make one peer look like several.
An endpoint accepting an anonymous session can establish nothing and cannot correlate it with any other, so it SHOULD assign a distinct ID per session: less than an identity, but enough to keep routes it attributed to that session from being advertised back to it, which is the loop 0 cannot prevent.

An assigned ID is indistinguishable on the wire from a declared one, so it identifies the peer to everyone the receiver forwards to; a peer that declared 0 for anonymity did not ask for that.


# Namespace Advertisements {#namespace}
This extension carries HOP_PATH and ROUTE_COST as Key-Value-Pair parameters ({{moqt}} Section 2.5).
PUBLISH_NAMESPACE ({{moqt}} Section 10.15) already has a Parameters field.

NAMESPACE ({{moqt}} Section 10.16) does not, and a subscriber-driven mesh propagates advertisements as NAMESPACE messages, so this extension defines an extended form used only on a session that negotiated the extension:

~~~
NAMESPACE Message (Cluster) {
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

## HOP_PATH Parameter {#hop-path}
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
Costs still accumulate across such a mesh, because each receiver adds the price for the direction it received over ({{relay-cost}}) regardless.

The original publisher seeds the value with its production cost: 0 for content it is already producing, higher for content it would have to spin up on demand, such as a standby transcoder advertising everything it *could* serve.


# Relay Behavior
When forwarding an advertisement downstream, a relay MUST append its own Hop ID to the HOP_PATH it received, so its own ID is always the last entry.
An advertisement arriving from an upstream that did not negotiate the extension has no HOP_PATH; the relay creates one containing a single entry for that upstream, 0 ({{zero}}) or an ID it assigned ({{assigned}}), then appends its own.

On receipt, a relay MUST discard an advertisement whose HOP_PATH already contains its own non-zero Hop ID: forwarding it would extend a loop, and subscribing through it would route the relay back to itself.
This receiver-side check catches loops of any length and is the only loop defense required.
A conforming sender never sends one ({{selection}}), so a receiver MAY instead close the session with a PROTOCOL_VIOLATION; discarding is what keeps a mesh working when one member does not conform.

## Accumulating Cost
A relay MUST add the cost the sending endpoint declared ({{relay-cost}}) to the ROUTE_COST it received before forwarding or acting on an advertisement.
The addition MUST saturate rather than wrap, so an absurd upstream value ranks last instead of overflowing to best.

A relay actively carrying the namespace (a live subscription exists for at least one of its tracks) SHOULD advertise 0 instead of the accumulated value: its ingress is already paid for, so one more subscriber costs only the links below it.
This is what lets a cluster deduplicate onto a warm copy.
The discount applies only to the advertisement for the path it actually serves from; a standby path keeps its accumulated value, since serving from it means opening a fresh ingest.
When it stops carrying the namespace it SHOULD restore the accumulated value, optionally after a grace period so brief churn does not flap routing.

Two relays that independently begin carrying the same namespace would each see the other's 0 as cheaper than its own source, and both switching at once would leave the namespace with no source.
Before re-parenting onto a 0-cost advertisement from another actively-carrying relay (one whose HOP_PATH has two or more entries), a relay SHOULD apply a deterministic tie-break, such as comparing a hash of the namespace and each Hop ID, so exactly one side moves.
Equal Hop IDs (including two relays that both declared 0) cannot be ordered, and neither side SHOULD move.
Cheaper advertisements from anything else carry no such hazard and SHOULD be adopted immediately.

## Updating an Advertisement {#updating}
An endpoint updates a PUBLISH_NAMESPACE with REQUEST_UPDATE ({{moqt}} Section 9.5) on its request stream, carrying the HOP_PATH or ROUTE_COST that changed.
A parameter omitted from REQUEST_UPDATE keeps its value, so a relay that starts carrying a namespace sends ROUTE_COST as an explicit 0 rather than omitting it.
The receiver answers REQUEST_OK, or REQUEST_ERROR and closes the stream, which withdraws the advertisement.

NAMESPACE is a response and has no REQUEST_UPDATE, so an endpoint updates one by re-sending it with new parameters on the SUBSCRIBE_NAMESPACE response stream that carries it.
A receiver MUST NOT treat the repeat as a duplicate or a protocol violation.

In {{moqt}} an advertisement lives for the lifetime of its stream, so an update on a *new* stream would leave two streams claiming one namespace and let the superseded one retract its replacement.
An endpoint MUST NOT open a second stream for a namespace it already advertises on this session.

Replacement is atomic, so a receiver MUST NOT tear down subscriptions or drop cached state merely because an update arrived.
What it means for existing subscriptions follows the first HOP_PATH entry ({{selection}}): unchanged, the content is continuous and subscriptions MAY resume on the new route at a group boundary; changed, a different publisher has taken over and they do not carry over.

This is the one comparison 0 ({{zero}}) does not decide: it identifies nothing, but there is one advertisement here and the stream carrying it is the continuity.
An endpoint whose publisher did change MUST withdraw the advertisement (NAMESPACE_DONE or PUBLISH_NAMESPACE_DONE) and advertise again rather than update in place.

The expected case is a ROUTE_COST-only change, which is how a relay signals that it started or stopped carrying the namespace.


# Path Selection {#selection}
A receiver holding advertisements for the same namespace over several sessions SHOULD prefer the lowest ROUTE_COST, breaking ties toward the shorter HOP_PATH and then toward the most recently received.
This is advisory: a receiver MAY apply local policy such as measured RTT instead.

Two advertisements whose HOP_PATH begins with the same non-zero Hop ID share a publisher and carry interchangeable content, so a receiver MAY hold them as redundant paths and fail an active subscription over to the survivor.
If the first entries differ, or either is 0, they are distinct publishers reusing a namespace: a receiver MUST NOT treat them as interchangeable and SHOULD treat the later as replacing the earlier.

A publisher MUST NOT advertise a path whose HOP_PATH contains the Hop ID that peer declared.
The receiver can only discard it, and acting on it would form a loop, so sending one is never useful.
Of the paths that remain a publisher SHOULD advertise the best, and advertises nothing when every known path contains that Hop ID.
Because selection is per session, a peer that the serving path flows through still receives the best standby, which is what lets it fail over if its own copy dies.

When serving a subscription, a publisher MUST select the source by that same rule.
If only excluded sources remain the subscription is unroutable, since serving it would hand the subscriber data that already flowed through itself.
Applying one rule to both advertisement and dispatch keeps advertised paths truthful and prevents subscription cycles of any length.


# One Publisher per Namespace {#single}
{{moqt}} allows several publishers to advertise one namespace and expects a relay to forward a matching SUBSCRIBE to each.
This extension does not: a session advertises a namespace at most once, a relay advertises only the best path it knows ({{selection}}), and a subscription is served from one source.

A relay holding paths to two publishers of one namespace has no good move.
SUBSCRIBE names a track, not a publisher, so a relay could only forward it along every path and deliver both publishers' Objects to every subscriber below it, or pick one path silently.
Two publishers also share no history, so nothing fetched from one is valid against the other.
Advertising a namespace once per publisher would only push the same choice one hop downstream.

A second publisher of a namespace therefore replaces the first rather than joining it ({{selection}}).
Redundant ingest of one content is a deployment concern outside this document.


# Security Considerations {#security}
A Hop ID reveals nothing beyond what its operator encodes in it, and a deployment that considers its identifiers sensitive can use random values or declare 0 ({{zero}}).
Declaring 0 hides an identity from the mesh but not from the peer itself, which MAY assign one and forward it onward ({{assigned}}); an endpoint that needs to stay unlinkable past its first hop cannot get that from this extension.
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
- Stated that a namespace is routed to one publisher: a session advertises it at most once and a second publisher replaces the first.
- Named the routing protocols whose single per-direction metric RELAY_COST follows.

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
