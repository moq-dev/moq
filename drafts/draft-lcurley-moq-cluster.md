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
An advertisement may also be a pattern over namespaces, so a service claims what it could serve without enumerating it.

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
The RELAY_COST Setup Option prices what subscribing from an endpoint costs, defaulting to 1 so an unpriced mesh simply ranks by hop count.
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
An endpoint MAY declare what subscribing from it costs:

~~~
RELAY_COST Setup Option {
  Option Key (vi64) = 0x40B56
  Option Value (vi64)
}
~~~

The option prices one direction, the sender's own egress, so each endpoint declares its own and the two need not match.
A receiver adds the value the sender declared to the ROUTE_COST of every advertisement that sender forwards.
An absent option means 1, under which the accumulated cost equals the hop count.
0 is meaningful and distinct from absent: it makes that direction free, which is how a deployment describes two relays in the same datacenter.

A declared cost is an assertion, not an instruction: a receiver MAY charge a locally configured value instead, so a peer cannot reprice its neighbours by declaring itself cheap.


# Hop IDs
A **Hop ID** is a variable-length integer identifying one endpoint within an advertisement's path.

Hop IDs SHOULD be unique among the endpoints an advertisement can traverse.
An endpoint MAY generate one randomly, since collisions across a 64-bit space are unlikely, or use a stable configured identifier that survives restarts.

Loop detection and origin identification compare Hop IDs for equality, so two endpoints sharing a Hop ID are indistinguishable.

## The Reserved Hop ID 0 {#zero}
**0 means "no identity"** and is reserved.
It is used for an endpoint that did not negotiate this extension, and an endpoint MAY also declare 0 to withhold its identity.

Because any number of endpoints can be 0, it identifies nothing, which constrains all three uses:

- **Loop detection**: 0 in a HOP_PATH is never a loop. A receiver whose own Hop ID is 0 cannot detect loops through itself, and MUST NOT discard an advertisement merely because the path contains 0.
- **Origin identity**: an advertisement whose first entry is 0 has an unknown origin. Updating one advertisement is not two ({{updating}}).
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
Sharing one suppresses each one's advertisements to the other, so two unrelated publishers would starve each other of routes.

How an endpoint scopes the ID follows from what it can establish about the peer.
One it authenticated, or one it dialed and therefore chose, SHOULD get a single stable ID; assigning per connection there would make one peer look like several.
An endpoint accepting an anonymous session can establish nothing and cannot correlate it with any other, so it SHOULD assign a distinct ID per session: less than an identity, but enough to keep routes it attributed to that session from being advertised back to it, which is the loop 0 cannot prevent.

An assigned ID is indistinguishable on the wire from a declared one, so it identifies the peer to everyone the receiver forwards to; a peer that declared 0 for anonymity did not ask for that.


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

An advertisement is a claim of capability, not inventory: it says namespaces beneath the advertised one can be served, never that any exists.
That is what makes a pattern advertisement ({{namespace-pattern}}) well-formed however wide it claims, and refusal ({{selection}}) how one namespace is denied.

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
A standby seed MUST exceed the largest accumulated cost a bounded HOP_PATH can carry, and `2^32` is RECOMMENDED; below that a nearby standby outranks a distant publisher already doing the work, and the mesh starts a second copy.

## NAMESPACE_PATTERN Parameter {#namespace-pattern}
NAMESPACE_PATTERN makes the advertised namespace a pattern: one Segment Kind per tuple field, in order.

~~~
NAMESPACE_PATTERN Parameter {
  Type (vi64) = 0x40B59
  Length (vi64)
  Segment Kind (vi64) ...
}
~~~

|------|----------|-------------|
| Kind | Name     | Tuple Field |
|-----:|:---------|:------------|
| 0x0  | Literal  | The field's bytes. Matches exactly that field. |
|------|----------|-------------|
| 0x1  | Wildcard | Empty. Matches any one field. |
|------|----------|-------------|
| 0x2  | Globstar | Empty. Matches any run of zero or more fields; at most one per namespace. |
|------|----------|-------------|

A namespace matches a pattern when its fields can be assigned to the pattern's segments in order, the globstar taking any number of them.
Every wildcard is a whole field, and a pattern is exact: without the parameter an advertisement covers its namespace and everything beneath it, which is the pattern ending in a globstar.
A receiver MUST close the session with a PROTOCOL_VIOLATION if the number of kinds differs from the number of tuple fields, a Wildcard or Globstar field is non-empty, or a second Globstar appears.
Other kinds are reserved for extensions: a receiver MUST NOT select or forward an advertisement carrying one, but MUST otherwise process the message.

The parameter is sent only on a session that negotiated Relay Hops.
{{moqt}} itself advertises only prefixes, so a pattern is not forwarded to a peer that did not negotiate this extension; it is simply not advertised there.
Like the Track Namespace Suffix it accompanies, the pattern is relative to the subscribed prefix, and rebasing a pattern under a prefix is set-valued: each way the globstar can align with the prefix yields a distinct residual, and the publisher sends each as its own advertisement.
A receiver MUST NOT present a pattern as a namespace that exists, and MUST discard a pattern advertisement not contained by what the sender may publish; how that authorization is expressed is out of scope.


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
An endpoint updates an advertisement by re-sending it with new parameters **on the stream that already carries it**: the original PUBLISH_NAMESPACE request stream, or the SUBSCRIBE_NAMESPACE response stream the NAMESPACE arrived on.
A receiver MUST NOT treat the repeat as a duplicate or a protocol violation.

In {{moqt}} an advertisement lives for the lifetime of its stream, so an update on a *new* stream would leave two streams claiming one namespace and let the superseded one retract its replacement.
An endpoint MUST NOT open a second stream for a namespace it already advertises on this session.

An update is metadata only: it re-prices or re-routes the advertisement and carries no content claim, so a receiver MUST NOT tear down subscriptions or drop cached state merely because one arrived.

The expected case is a ROUTE_COST-only change, which is how a relay signals that it started or stopped carrying the namespace.


# Path Selection {#selection}
A receiver resolving a request against the advertisements covering its namespace, prefixes and patterns alike, consults only the most specific.
Specificity is structural: more literal fields first, then no globstar over one, then more wildcards, then a longer literal head, so an advertisement strictly inside another's namespaces ranks above it, a concrete namespace shadows every pattern covering it, and equally specific advertisements form one tier.
A refusal from that tier never falls through to a less specific one.

Within the tier, a receiver SHOULD prefer the lowest ROUTE_COST, breaking ties toward the shorter HOP_PATH and then toward the most recently received.
Pattern advertisements tied at the lowest cost are a pool: a deterministic hash of the requested namespace against each advertiser distributes distinct namespaces across them.
The hash is FNV-1a from the basis `0x420C0DECB00B`: for each byte of the requested namespace's fields joined by `/`, then each of the eight little-endian bytes of the advertiser's first Hop ID, XOR the byte in and multiply by `0x100000001B3`, wrapping at 64 bits; the highest result wins, and a first Hop ID of 0 makes the advertisement a pool member of its own.
This is advisory: a receiver MAY apply local policy such as measured RTT instead.

An advertiser that will not serve a request resolved against its pattern refuses it with an error code.
NO_CAPACITY ({{iana}}) permits the receiver ONE re-resolution within the same tier, excluding that advertiser; the exclusion is what makes the retry safe, since a retraction and a request for the slot it gave away necessarily cross.
A receiver that has spent that retry, or has nothing to spend it on, MUST refuse downstream with another code, so the retry cannot compound hop by hop.
Every other code, and any unrecognized one, is terminal.
A relay MUST NOT advertise a namespace merely because it resolved it; the advertiser announces the concrete namespace once it is producing.

An advertisement carries no content identity: nothing promises that two paths to one namespace serve interchangeable bytes.
A receiver MUST NOT splice an active subscription across sessions; when the serving session's advertisement goes away, subscriptions through it end, and the receiver re-subscribes through the best remaining path.

A publisher MUST NOT advertise a path whose HOP_PATH contains the Hop ID that peer declared.
The receiver can only discard it, and acting on it would form a loop, so sending one is never useful.
Of the paths that remain a publisher SHOULD advertise the best, and advertises nothing when every known path contains that Hop ID.
Because selection is per session, a peer that the serving path flows through still receives the best standby, which is what lets it fail over if its own copy dies.

When serving a subscription, a publisher MUST select the source by that same rule.
If only excluded sources remain the subscription is unroutable, since serving it would hand the subscriber data that already flowed through itself.
Applying one rule to both advertisement and dispatch keeps advertised paths truthful and prevents subscription cycles of any length.


# Security Considerations
A Hop ID reveals nothing beyond what its operator encodes in it, and a deployment that considers its identifiers sensitive can use random values or declare 0 ({{zero}}).
Declaring 0 hides an identity from the mesh but not from the peer itself, which MAY assign one and forward it onward ({{assigned}}); an endpoint that needs to stay unlinkable past its first hop cannot get that from this extension.
A HOP_PATH does expose how many hops an advertisement crossed, which hints at the size of a deployment; a relay MAY coalesce its internal hops into one entry, or strip HOP_PATH, before forwarding across a trust boundary.

Because a relay only appends to HOP_PATH, it cannot make a competing path look shorter than it is; the worst it can do is under-report its own upstream portion to win an advisory tie-break.
ROUTE_COST has no such structural protection: it is a single value the sender chooses, so a relay can advertise 0 for content it is not carrying and attract subscriptions it then has to fetch.
Both cost only a suboptimal path choice, and the latter is self-limiting, since the traffic won this way must then be served.

A receiver MUST NOT make security decisions based on Hop IDs, and a deployment spanning a trust boundary SHOULD treat a peer's ROUTE_COST as a hint to clamp or ignore rather than an accounting figure.


# IANA Considerations {#iana}

This document requests the following registrations.
High, distinctive values are requested to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions.

## MOQT Setup Options

This document requests two registrations in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required.

| Value   | Name       | Reference     |
|:--------|:-----------|:--------------|
| 0x40B55 | RELAY_HOPS | This Document |
| 0x40B56 | RELAY_COST | This Document |

## MOQT Message Parameters

This document requests three registrations in the "MOQT Message Parameters" registry ({{moqt}} Section 15.7).
All are carried in PUBLISH_NAMESPACE and in the extended NAMESPACE message ({{namespace}}).

| Value   | Name              | Carried In                   | Reference     |
|:--------|:------------------|:-----------------------------|:--------------|
| 0x40B57 | HOP_PATH          | PUBLISH_NAMESPACE, NAMESPACE | This Document |
| 0x40B58 | ROUTE_COST        | PUBLISH_NAMESPACE, NAMESPACE | This Document |
| 0x40B59 | NAMESPACE_PATTERN | PUBLISH_NAMESPACE, NAMESPACE | This Document |

The Key-Value-Pair parity is load-bearing: HOP_PATH, NAMESPACE_PATTERN, and RELAY_HOPS are odd, so their values are length-prefixed byte strings, while ROUTE_COST and RELAY_COST are even, so their values are bare varints.

## MOQT Error Codes

This document requests one registration in the "REQUEST_ERROR Codes" registry ({{moqt}} Section 15.11.2).

| Value   | Name        | Reference     |
|:--------|:------------|:--------------|
| 0x40B5A | NO_CAPACITY | This Document |

NO_CAPACITY refuses a request the publisher could serve but has no capacity for now ({{selection}}).


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
