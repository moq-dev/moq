---
title: "MoQ Broadcast Extension"
abbrev: "moq-broadcast"
category: info

docname: draft-lcurley-moq-broadcast-latest
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
  relay-hops: I-D.lcurley-moq-relay-hops

informative:

--- abstract

This document defines a Broadcast extension for MoQ Transport {{moqt}}.
Each namespace advertisement carries an Epoch identifying the generation of content published under the namespace.
Receivers resolve competing advertisements by Epoch rather than arrival order, and a subscriber can pin a subscription or fetch to a specific generation.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}

A **broadcast** is one generation of the content published under a namespace: when a publisher restarts with new content, or a different publisher takes over the name, a new broadcast occupies the same namespace.


# Introduction
{{moqt}} identifies content by namespace, but a namespace is reused across time: a restarted or replacement publisher advertises the same name for entirely new content.
A receiver cannot tell that apart from the same content arriving over another path, so implementations fall back to preferring the most recently received advertisement, which is a race: a stale advertisement on a fresh session looks newer than the generation it predates.
The origin identity of {{relay-hops}} does not close the gap either: the same publisher producing new content after a restart is indistinguishable from a route change.

This extension adds an **Epoch** to each namespace advertisement.
The original publisher assigns it, relays forward it unchanged, and a larger value identifies a newer generation, so replacement is decided by value rather than arrival order.
A subscriber can also echo the Epoch in a subscription or fetch, turning "give me the content I saw advertised" into a check the publisher enforces.


# Setup Negotiation
The Broadcast extension is negotiated during the SETUP exchange as defined in {{moqt}} Section 10.3.
An endpoint indicates support by including the following Setup Option; it carries no value.

~~~
BROADCAST Setup Option {
  Option Key (vi64) = 0x40B59
  Option Value Length (vi64) = 0
}
~~~

The extension is negotiated independently per session.
Negotiating it also enables the extended NAMESPACE message format of {{relay-hops}}, which appends a Parameters field to NAMESPACE; the appended field carries whichever parameters the negotiated extensions define.

Message parameters in {{moqt}} have no generic skip rule, so an endpoint MUST NOT send EPOCH on a session that did not negotiate this extension.
A relay forwarding into such a session strips EPOCH, and downstream receivers treat the advertisement as unspecified (see [Unspecified Epochs](#unspecified-epochs)).


# EPOCH Parameter
The EPOCH parameter carries the generation of the broadcast.
It is a parameter (see {{moqt}} Section 2.5) carried in a namespace advertisement (PUBLISH_NAMESPACE, {{moqt}} Section 10.15, or an extended NAMESPACE, {{relay-hops}}), and in SUBSCRIBE and FETCH to pin the request (see [Pinning Subscriptions and Fetches](#pinning-subscriptions-and-fetches)).

~~~
EPOCH Parameter {
  Type (vi64) = 0x40B5B
  Length (vi64)
  Epoch (vi64)
}
~~~

**Epoch**:
The generation of content at this namespace, chosen by the original publisher and forwarded unchanged by relays.
Each new generation MUST use a non-zero Epoch greater than the last.
Wall-clock milliseconds are a convenient source, but clocks roll back and skew, so a publisher SHOULD take the maximum of its clock and one more than the highest Epoch it can observe at the namespace.
A violation is not fatal: receivers keep no high-water mark, so an erroneously high Epoch suppresses newer generations only while its advertisement remains available.
A value of 0 is equivalent to omitting the parameter.

## Selection by Epoch
A receiver holding advertisements for the same namespace MUST prefer the highest Epoch (a specified Epoch outranks an unspecified one): a lower Epoch is a stale generation, never an alternate path, regardless of arrival order or path length.
A relay SHOULD end its advertisement of a lower generation once it holds a higher one, rather than wait for it to end on its own, which would hold the namespace for however long the transport takes to notice a publisher is gone.

Advertisements with the same non-zero Epoch carry interchangeable content: a receiver MAY hold them as redundant paths and switch between them, including failing an active subscription over when the serving path ends.
Cooperating redundant publishers opt in by minting the same Epoch, e.g. derived from the event rather than from each process.
Any other pair is two generations: cached immutable track properties MUST be discarded on replacement, and existing subscriptions do not carry over.

When combined with {{relay-hops}}, Epoch comparison happens first; its path-length tie-break and origin-identity rules apply only among advertisements with the same Epoch.

## Unspecified Epochs
An advertisement without EPOCH carries no generation: the publisher predates this extension, or the parameter was stripped crossing a non-supporting session.
Unspecified advertisements are never interchangeable with specified ones; among themselves, the identity rules otherwise in effect apply ({{relay-hops}} origin identity, or plain {{moqt}} semantics).

## Pinning Subscriptions and Fetches
On a session that negotiated this extension, a subscriber MAY include EPOCH in SUBSCRIBE or FETCH; the request then targets exactly that generation, and the publisher MUST reject it rather than serve a different one.
A publisher that retains an older generation (e.g. a recording) MAY serve a FETCH pinned to it even after a newer generation replaced the namespace.
A request without EPOCH targets whatever generation the publisher currently serves, matching {{moqt}}'s default behavior.
Echoing the Epoch of the advertisement acted on closes the race where a request crosses a replacement in flight.


# Security Considerations
A wall-clock-derived Epoch reveals approximately when a broadcast started; a publisher that considers this sensitive can use any other increasing scheme.

A misbehaving publisher or relay can advertise an arbitrarily high Epoch and suppress legitimate content at that namespace, but only while its advertisement remains available, since receivers keep no high-water mark.
This is the same trust already placed in namespace advertisements themselves; receivers MUST NOT treat an Epoch as proof of freshness or authenticity.


# IANA Considerations

This document requests the following registrations.
High, distinctive values are requested to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions.

## MOQT Setup Options

This document requests a registration in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required.

| Value   | Name      | Reference     |
|:--------|:----------|:--------------|
| 0x40B59 | BROADCAST | This Document |

## MOQT Message Parameters

This document requests a registration in the "MOQT Message Parameters" registry ({{moqt}} Section 15.7).

| Value   | Name  | Carried In                                    | Reference     |
|:--------|:------|:----------------------------------------------|:--------------|
| 0x40B5B | EPOCH | PUBLISH_NAMESPACE, NAMESPACE, SUBSCRIBE, FETCH | This Document |


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
