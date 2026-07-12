---
title: "MoQ Route Cost Extension"
abbrev: "moq-route-cost"
category: info

docname: draft-lcurley-moq-route-cost-latest
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
  hops: I-D.lcurley-moq-relay-hops

--- abstract

This document defines a Route Cost extension for MoQ Transport {{moqt}}.
Each namespace advertisement carries a route cost that a relay accumulates as the advertisement propagates: a base cost set once by the origin plus a transit cost that each relay increases by the cost of the link the advertisement crossed.
A relay or subscriber that sees the same namespace advertised over multiple paths prefers the one with the lowest total cost, generalizing the shortest-path preference of the Relay Hops extension {{hops}} to weighted links.
A relay that is already forwarding a broadcast advertises a transit cost of zero for it, so downstream receivers prefer a path through a relay that already carries the media (its upstream is already paid for) over opening a redundant one.
The cost of a live advertisement can be updated in place without re-advertising the path.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}


# Introduction
{{moqt}} delivers content end-to-end through a mesh of relays.
When the same namespace can reach a relay over more than one path, the relay (or a subscriber) must choose which path to pull from.
The Relay Hops extension {{hops}} lets a receiver prefer the path with the fewest hops, which is a good proxy for latency but treats every hop as equal.

Real deployments are not uniform:

- **Links have different costs.** A hop between two relays in the same datacenter is effectively free, while a hop across a metered backbone link is expensive. A receiver that counts only hops cannot tell a cheap two-hop path from an expensive one-hop path.
- **A path may already be warm.** When a relay is already forwarding a broadcast to some subscriber, another relay pulling that same broadcast *through* it adds no new upstream transfer, because the media is already flowing. Hop count alone cannot express that a path through an already-serving relay is cheaper than a fresh one of equal length.
- **A source may carry a standing preference.** An operator may wish to steer traffic toward or away from a particular origin regardless of distance (for example, to prefer a transcoded rendition over transcoding again downstream).

This extension addresses all three by replacing the hop *count* with an additive **route cost**.
Every namespace advertisement carries a cost with two components: a **base cost** the origin sets once and every relay forwards unchanged, and a **transit cost** each relay increases by the cost it assigns to the link the advertisement arrived on.
A receiver prefers the advertisement with the lowest total cost.

The extension is deliberately a superset of {{hops}}: when every link costs 1 and no relay applies the marginal-cost rule below, the transit cost equals the hop count and route-cost selection reduces exactly to shortest-path selection.
A receiver MAY default an advertisement that carries no route cost to a transit cost equal to its {{hops}} path length, so a mixed deployment where only some relays support this extension still orders paths sensibly.

## Marginal cost
The key departure from a static metric is that a relay's advertised transit cost depends on what it is currently serving.
A relay that is **actively forwarding** a broadcast (it holds a live subscription to it on behalf of some downstream receiver) advertises a transit cost of **zero** for that broadcast when forwarding, because a further downstream receiver pulling through it costs nothing new upstream: the media is already in flight.
This is a marginal-cost signal, not a total-cost one, and it is what lets a mesh converge onto shared paths — cache-aware routing — rather than each receiver independently opening its own expensive path to the origin.
Because the signal changes as subscriptions come and go, the cost of a live advertisement can change during its lifetime; see [Cost Updates](#cost-updates).


# Setup Negotiation
The Route Cost extension is negotiated during the SETUP exchange as defined in {{moqt}} Section 10.3.
An endpoint indicates support, and declares the cost of the link this session represents, with the following Setup Option:

~~~
ROUTE_COST Setup Option {
  Option Key (vi64) = 0x40C05
  Option Value Length (vi64)
  Link Cost (vi64)
}
~~~

**Link Cost**:
The cost this endpoint assigns to the link carried by this session, applied by the peer to the transit cost of every advertisement it forwards that arrived over this session (see [Relay Behavior](#relay-behavior)).
A value of 1 is the neutral default that reproduces hop-count behavior; 0 marks a free link (for example, two relays in the same datacenter) that routing should prefer.
The link cost is a property of the connection, so the dialing side declares it and both endpoints apply the same value; if both endpoints declare one, an endpoint MUST use the value its peer advertised for advertisements it receives on this session, so the two directions agree.

The extension applies to a single hop (one MOQT session) and is negotiated independently for each session.
Because this extension carries its state as a parameter on namespace advertisements (see [ROUTE_COST Parameter](#route_cost-parameter)) and per {{moqt}} an unknown Key-Value-Pair Type is ignored, an advertisement forwarded into a session that did not negotiate the extension simply loses its cost gracefully; the receiver falls back to hop-count or arrival-order selection.

This extension composes with, but does not require, the Relay Hops extension {{hops}}.
When both are negotiated, hops are still used for loop detection and origin identity, and the route cost supersedes the hop count for path selection.
When only Route Cost is negotiated, a receiver still needs some means of loop detection; {{hops}} is RECOMMENDED alongside it.


# Route Cost
A **route cost** is a pair of variable-length integers, `(Base Cost, Transit Cost)`, whose sum is the value a receiver compares when choosing among paths.

**Base Cost** is set once by the origin publisher and forwarded by every relay unchanged.
It is a standing penalty (or, at 0, a neutral default) attached to a source regardless of how far the advertisement travels.

**Transit Cost** accumulates along the path.
Each relay that forwards an advertisement increases the transit cost by the [Link Cost](#setup-negotiation) of the session the advertisement arrived on — except that a relay actively forwarding the broadcast advertises a transit cost of 0 (see [Marginal cost](#marginal-cost)).

The total cost is `Base Cost + Transit Cost`, computed with saturating addition so that it never wraps.
A receiver prefers the advertisement with the lowest total cost.


# ROUTE_COST Parameter
The ROUTE_COST parameter carries the route cost of an advertisement.
It is a Key-Value-Pair (see {{moqt}} Section 2.5) carried in the Parameters of a namespace advertisement: a PUBLISH_NAMESPACE message ({{moqt}} Section 10.15) or, when {{hops}} is also negotiated, the extended NAMESPACE message it defines.
An endpoint that negotiated this extension but not {{hops}} on a NAMESPACE-carrying session uses the same extended NAMESPACE format (a trailing Parameters field) to carry ROUTE_COST.

Because the value is two integers, ROUTE_COST uses an odd Type so that it is length-prefixed:

~~~
ROUTE_COST Parameter {
  Type (vi64) = 0x40C07
  Length (vi64)
  Base Cost (vi64)
  Transit Cost (vi64)
}
~~~

**Base Cost**:
The origin-set base cost, forwarded unchanged.

**Transit Cost**:
The accumulated transit cost as seen by the receiver, i.e. after the sending relay has applied its forwarding rule.

A receiver MUST close the session with a PROTOCOL_VIOLATION if the two integers do not exactly fill `Length`.
An advertisement that omits ROUTE_COST is treated as having a base cost of 0 and a transit cost equal to its {{hops}} path length (or, absent {{hops}}, left to local policy).

## Relay Behavior
When a relay forwards a namespace advertisement downstream on a session that negotiated this extension, it MUST set the ROUTE_COST it sends as follows:

- **Base Cost**: copied unchanged from the advertisement it received (0 if none).
- **Transit Cost**: if the relay is currently forwarding an active subscription to this broadcast, `0`; otherwise the received transit cost plus the [Link Cost](#setup-negotiation) of the session the advertisement arrived on.

A relay MUST NOT decrease the base cost, and MUST NOT decrease the transit cost except by the marginal-cost rule (setting it to 0 while actively forwarding).
These bounds ensure that a relay cannot make a competing path look arbitrarily cheap.

## Cost Updates
Unlike the hop chain, which is fixed for the lifetime of an advertisement, the route cost can change while the advertisement is live: the marginal-cost signal flips as the relay starts or stops actively forwarding the broadcast, and an upstream cost change propagates hop by hop.

An endpoint MAY update the cost of a live advertisement by re-advertising the namespace with an updated ROUTE_COST; the re-advertisement replaces the prior one per the namespace-advertisement semantics of {{moqt}}, exactly as {{hops}} updates a path.
Because re-advertising resends the whole advertisement (and, downstream, may interrupt in-flight subscriptions that depend on it), a profile of this extension MAY instead define a lightweight cost-only update message that carries just the namespace and a new ROUTE_COST, leaving the rest of the advertisement (including the hop chain) untouched.
Such an update MUST NOT change any field other than the route cost; a receiver applies it by replacing the stored cost and re-evaluating path selection, without treating the advertisement as withdrawn and re-announced.

An endpoint SHOULD damp cost updates so that transient subscription churn does not cause a storm of updates or route flapping:

- A cost **decrease** (a path becoming cheaper, e.g. a relay starting to actively forward) MAY be signaled promptly, since converging onto a cheaper shared path is the goal.
- A cost **increase** (a path becoming more expensive, e.g. the last subscriber leaving so the relay stops actively forwarding) SHOULD be held for a short interval and re-checked before being signaled, so a brief gap between subscribers does not thrash the route.

A receiver that reacts to a cost decrease by switching paths SHOULD introduce a small, deterministic per-node delay before switching, so that two relays whose preferences would mutually flip toward each other do not switch simultaneously; the first to switch suppresses the other's now-looping candidate (via {{hops}} loop detection), and the pair converges.

## Path Selection
A relay or subscriber that receives advertisements for the same namespace over multiple sessions SHOULD prefer the advertisement with the lowest total cost (`Base Cost + Transit Cost`).
When two advertisements have equal total cost, the receiver breaks the tie by any deterministic, path-stable rule so that every node in the mesh converges on the same choice; a hash of the namespace and hop chain (as in {{hops}}) is RECOMMENDED, since it spreads equal-cost paths across upstreams rather than funneling them onto one.

Path selection remains advisory: a receiver MAY apply additional local policy (measured RTT, administrative preference) on top of the advertised cost.

A publisher (or relay acting as one) SHOULD advertise only the single best path it currently knows for each namespace, updating the advertised cost as described in [Cost Updates](#cost-updates) rather than advertising several competing paths at once.


# Security Considerations
The route cost is a set of opaque integers that a relay accumulates; like the hop count in {{hops}}, it can leak coarse information about the size and cost structure of a deployment. A relay that wishes to hide its internal topology MAY normalize the cost (for example, collapse the transit accumulated within its own administrative domain into a single value) before forwarding across a trust boundary.

Because a relay may only increase the base and transit costs — never decrease them, except by the well-defined marginal-cost rule — a misbehaving relay cannot make a competing path appear arbitrarily cheap; the worst it can do is under-report its own transit (claim to be actively forwarding when it is not) to win an advisory tie-break, at most steering traffic onto a suboptimal but still loop-free path. Since path selection is advisory and loop detection is provided separately by {{hops}}, the impact is limited to a poor path choice, not a routing loop or a denial of service. A receiver MUST NOT make security decisions based on the advertised cost, and SHOULD corroborate it with locally measured signals when it matters.

A relay MUST bound the rate at which it acts on cost updates (see [Cost Updates](#cost-updates)) so that a peer flapping an advertisement's cost cannot force unbounded re-evaluation or a storm of downstream updates.


# IANA Considerations

This document requests the following registrations.
High, distinctive values are requested to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions.
ROUTE_COST carries a two-integer value, so its Type is odd (length-prefixed). See {{moqt}} Section 2.5.

## MOQT Setup Options

This document requests a registration in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required.

| Value   | Name       | Reference     |
|:--------|:-----------|:--------------|
| 0x40C05 | ROUTE_COST | This Document |

## MOQT Message Parameters

This document requests a registration in the "MOQT Message Parameters" registry ({{moqt}} Section 15.7).
ROUTE_COST is carried in PUBLISH_NAMESPACE and in the extended NAMESPACE message defined by {{hops}}.

| Value   | Name       | Carried In                   | Reference     |
|:--------|:-----------|:-----------------------------|:--------------|
| 0x40C07 | ROUTE_COST | PUBLISH_NAMESPACE, NAMESPACE | This Document |


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
