---
title: "MoQ Dynamic Extension"
abbrev: "moq-dynamic"
category: info

docname: draft-lcurley-moq-dynamic-latest
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
  cluster: I-D.lcurley-moq-cluster

informative:

--- abstract

This document defines a dynamic advertisement extension for MoQ Transport {{moqt}}.
A publisher advertises a pattern of namespaces it could produce on demand, a prefix and a suffix with either possibly empty, instead of enumerating every namespace it could serve.
A relay routes a request for an unadvertised namespace to the best matching advertiser, which produces the content or refuses the request.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}

A **dynamic advertisement** is a PUBLISH_NAMESPACE or NAMESPACE message carrying the DYNAMIC_SUFFIX parameter ({{suffix}}).
It advertises a pattern of namespaces rather than an exact one.


# Introduction
Some content does not exist until somebody asks for it, and the set of namespaces it could occupy is too large to enumerate: a standby transcoder that would otherwise advertise a derivative per broadcast, or an archive whose claim is "if nobody is producing this live, I have it".
A dynamic advertisement makes the claim once, as a pattern of namespaces the publisher *could* serve: a capability, not an inventory, with one namespace denied by refusing its request ({{refusal}}).


# Setup Negotiation
An endpoint indicates support with the following Setup Option ({{moqt}} Section 10.3), whose value is empty:

~~~
DYNAMIC Setup Option {
  Option Key (vi64) = 0x40B5D
  Option Value Length (vi64) = 0
}
~~~

An endpoint MUST NOT send a dynamic advertisement to a peer that did not declare support: without this extension a receiver reads the message as a concrete advertisement of the prefix itself, and would route requests for a namespace that does not exist.


# The DYNAMIC_SUFFIX Parameter {#suffix}
DYNAMIC_SUFFIX is a Key-Value-Pair parameter ({{moqt}} Section 2.5) carried in PUBLISH_NAMESPACE, and in the extended NAMESPACE message when {{cluster}} is also negotiated.
Its presence marks the advertisement dynamic.

~~~
DYNAMIC_SUFFIX Parameter {
  Type (vi64) = 0x40B5F
  Length (vi64)
  Track Namespace Suffix (..)
}
~~~

The message's namespace is the pattern's prefix and the parameter is its suffix: a namespace matches when it starts with the prefix and ends with the suffix, matching whole tuple fields, with the two not overlapping.
Either half may be empty, and both empty matches every namespace; the archive case above is exactly that.

A dynamic advertisement names no content.
A receiver MUST NOT present one as an available namespace, and SHOULD present duplicate advertisements of one pattern combined, gone only when the last advertiser withdraws.
An advertisement is withdrawn or replaced through the same forms a concrete advertisement uses.


# Selection {#selection}
An endpoint MAY resolve a request (SUBSCRIBE, FETCH, or TRACK_STATUS) for an unadvertised namespace against the dynamic advertisements it holds, and resolution recurses: a resolved request forwarded onward is still unadvertised there.
Resolution across more than one hop requires {{cluster}}: HOP_PATH is the only loop defense, so an endpoint that did not negotiate it MUST NOT resolve a request it is itself forwarding, and MUST NOT resolve a request onto the session it arrived from.
When {{cluster}} is negotiated, its HOP_PATH and ROUTE_COST rules apply to dynamic advertisements unchanged: relays append their hop when forwarding, discard an advertisement whose path contains their own Hop ID, exclude candidates whose path contains the requesting session's Hop ID, and accumulate cost.

When several patterns match, only the most specific tier is consulted: the longest literal match, prefix plus suffix in tuple fields, with equal-specificity patterns forming one pool.
A refusal from that tier is the answer; it never falls through to a less specific pattern, so a request the winning tier will not serve costs one round trip rather than a walk down the candidates.

Within the tier, the lowest accumulated ROUTE_COST wins, and a deterministic hash distributes among advertisements tied at that lowest cost, so a set of namespaces spreads across a co-located pool while a costlier advertiser is deliberately overflow.
The hash is rendezvous-style FNV-1a: for each candidate, hash the requested namespace's encoded fields followed by the advertiser identity (the first HOP_PATH entry, or a per-session value when absent or 0) as a 64-bit little-endian value, seeded with 0x420C0DECB00B, and select the highest result.
For dynamic advertisements this hash replaces {{cluster}}'s shorter-HOP_PATH and recency tie-breaks.

A resolved namespace competes on that same accumulated cost against any concrete advertisement of it; the kind carries no rank of its own.
A dynamic advertisement's cost is its on-demand production cost: it never takes the actively-carrying discount, and a publisher advertising standby capacity MUST price it above the largest accumulated topology cost its deployment can produce, or a nearby standby outranks a distant publisher already doing the work and the mesh starts a duplicate.


# Refusal {#refusal}
An advertiser that will not serve a resolved request refuses it with the error mechanism the request already has, and the code decides what the resolving endpoint may do next.

A DYNAMIC_CAPACITY refusal ({{iana}}) permits ONE re-resolution, excluding every advertisement with the refusing advertiser's identity: an advertiser's capacity and its peers' view of it are at least half a round trip apart, so a withdrawal and a request for the slot it just gave away necessarily cross, and the exclusion is what makes the retry safe.
Re-resolution may find no other advertiser, leaving the request unserved; that is a correct outcome, not a fallback list.
An endpoint that has spent its re-resolution, or has nothing to spend it on, MUST refuse the downstream request with a different, terminal code, so the single retry cannot compound hop by hop.

Every other refusal, and any code the endpoint does not recognize, is terminal and propagates without re-resolution.
An endpoint SHOULD NOT cache refusals; rate limiting is the advertiser's concern.


# Authorization
A receiver MUST discard a dynamic advertisement whose pattern prefix is not within what the sender may publish; how that authority is expressed is out of scope.
The suffix claims nothing about where a namespace begins, so an empty prefix claims every namespace and demands that authority.


# Security Considerations
An over-claiming pattern attracts requests its sender must then serve or refuse, so inflating a claim is self-limiting; the damage of a hostile claim is bounded by the authorization check above.
A receiver spanning a trust boundary SHOULD treat a peer's patterns like its costs: hints to clamp or ignore rather than facts.


# IANA Considerations {#iana}

This document requests the following registrations, using high, distinctive values to avoid the low ranges reserved by {{moqt}} and the values sibling extensions already claim.

## MOQT Setup Options

One registration in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required:

| Value   | Name    | Reference     |
|:--------|:--------|:--------------|
| 0x40B5D | DYNAMIC | This Document |

## MOQT Message Parameters

One registration in the "MOQT Message Parameters" registry ({{moqt}} Section 15.7), whose policy is Specification Required:

| Value   | Name           | Carried In                   | Reference     |
|:--------|:---------------|:-----------------------------|:--------------|
| 0x40B5F | DYNAMIC_SUFFIX | PUBLISH_NAMESPACE, NAMESPACE | This Document |

Both keys are odd, so their values are length-prefixed (an empty value for DYNAMIC, a byte string for DYNAMIC_SUFFIX), matching the Key-Value-Pair parity rule.

## MOQT Error Codes

One registration in the "MOQT Error Codes" registry:

| Value   | Name             | Reference     |
|:--------|:-----------------|:--------------|
| 0x40B5C | DYNAMIC_CAPACITY | This Document |

The advertiser lacks the capacity to take on a dynamically resolved request; the resolving endpoint MAY re-resolve once (see {{refusal}}).
On any other request the code is terminal, like any refusal.


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
