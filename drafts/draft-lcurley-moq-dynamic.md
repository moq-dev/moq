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

informative:
  cluster: I-D.lcurley-moq-cluster

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
Some content does not exist until somebody asks for it, and the set of namespaces it could occupy is too large to enumerate.
A standby transcoder would advertise a derivative for every broadcast it could transcode, so advertisements scale as workers times broadcasts.
A chat backend serves a room per user, whether or not anything else is live, which is not a set an advertisement per room can express.
An archive serving recordings wants to say "if nobody is producing this live, I have it", which is a claim about every namespace at once.

A dynamic advertisement makes the claim once: a pattern of namespaces the publisher *could* serve.
It is a capability, not an inventory.
It never implies that a matching namespace exists; a publisher denies one namespace by refusing its request ({{refusal}}), which is what makes an over-claiming pattern well-formed.


# Setup Negotiation
An endpoint indicates support with the following Setup Option ({{moqt}} Section 10.3), whose value is empty:

~~~
DYNAMIC Setup Option {
  Option Key (vi64) = 0x40B5B
  Option Value Length (vi64) = 0
}
~~~

An endpoint MUST NOT send a dynamic advertisement to a peer that did not declare support: without this extension a receiver reads the message as a concrete advertisement of the prefix itself, and would route requests for a namespace that does not exist.


# The DYNAMIC_SUFFIX Parameter {#suffix}
DYNAMIC_SUFFIX is a Key-Value-Pair parameter ({{moqt}} Section 2.5) carried in PUBLISH_NAMESPACE, and in the extended NAMESPACE message when {{cluster}} is also negotiated.
Its presence marks the advertisement dynamic.

~~~
DYNAMIC_SUFFIX Parameter {
  Type (vi64) = 0x40B59
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
A relay MAY resolve a request (SUBSCRIBE, FETCH, or TRACK_STATUS) for an unadvertised namespace against the dynamic advertisements it holds.

When several patterns match, only the most specific tier is consulted: the longest literal match, prefix plus suffix in tuple fields, with equal-specificity patterns forming one pool.
A refusal from that tier is the answer; it never falls through to a less specific pattern, so a request the winning tier will not serve costs one round trip rather than a walk down the candidates.

Within the tier, a deterministic hash of the *requested* namespace against each advertiser distributes the pool, so a set of namespaces spreads across the advertisers while one namespace always resolves the same way.
Hashing the pattern instead would hand one advertiser everything it matches.

When {{cluster}} is negotiated, its accumulated ROUTE_COST orders the pool before the hash, and a resolved namespace competes on that same cost against any concrete advertisement of it; nothing ranks the two kinds differently.
A dynamic advertisement's cost is its on-demand production cost: it never takes the actively-carrying discount, and a publisher advertising standby capacity MUST seed it above the largest accumulated topology cost the deployment can produce, or a nearby standby outranks a distant publisher already doing the work and the mesh starts a duplicate.
Without {{cluster}}, ordering within the tier is local policy.


# Refusal {#refusal}
An advertiser that will not serve a resolved request refuses it with the error mechanism the request already has, and the code decides what the relay may do next.

A capacity refusal permits the relay ONE re-resolution, within the same tier and excluding the refuser.
This race is inherent rather than an error: an advertiser's capacity and a relay's view of it are at least half a round trip apart, so a withdrawal and a request for the slot it just gave away necessarily cross.
The exclusion is what makes the retry safe, not the withdrawal arriving first, and re-resolution may find no other advertiser, leaving the request unserved; that is a correct outcome, not a fallback list.

Every other refusal, and any code the relay does not recognize, is terminal and propagates without re-resolution.
A relay SHOULD NOT cache refusals; rate limiting is the advertiser's concern.


# Authorization
A dynamic advertisement's prefix MUST be within what the sender is authorized to publish; how that authorization is expressed is out of scope.
The suffix claims nothing about where a namespace begins, so any pattern with an empty prefix asserts authority over every namespace and demands it.
Note that a mis-scoped empty prefix does not error; it quietly starts answering for everything, which is why the check belongs on the receiver.


# Security Considerations
An over-claiming pattern attracts requests its sender must then serve or refuse, so inflating a claim is self-limiting; the damage of a hostile claim is bounded by the authorization check above.
A receiver spanning a trust boundary SHOULD treat a peer's patterns like its costs: hints to clamp or ignore rather than facts.


# IANA Considerations

This document requests the following registrations, using high, distinctive values to avoid the low ranges reserved by {{moqt}}.

## MOQT Setup Options

One registration in the "MOQT Setup Options" registry ({{moqt}} Section 15.4):

| Value   | Name    | Reference     |
|:--------|:--------|:--------------|
| 0x40B5B | DYNAMIC | This Document |

## MOQT Message Parameters

One registration in the "MOQT Message Parameters" registry ({{moqt}} Section 15.7):

| Value   | Name           | Carried In                   | Reference     |
|:--------|:---------------|:-----------------------------|:--------------|
| 0x40B59 | DYNAMIC_SUFFIX | PUBLISH_NAMESPACE, NAMESPACE | This Document |

Both keys are odd, so their values are length-prefixed (an empty value for DYNAMIC, a byte string for DYNAMIC_SUFFIX), matching the Key-Value-Pair parity rule.


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
