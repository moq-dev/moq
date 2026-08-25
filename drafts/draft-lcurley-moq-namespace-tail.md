---
title: "MoQ Namespace Tail Extension"
abbrev: "moq-namespace-tail"
category: info

docname: draft-lcurley-moq-namespace-tail-latest
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

This document defines an extension for MoQ Transport {{moqt}} that lets a SUBSCRIBE_NAMESPACE carry a tail as well as a prefix.
A subscriber asks for the namespaces that both start with the prefix and end with the tail, and the publisher sends only those, rather than sending the whole set under the prefix for the subscriber to discard most of.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}

A namespace **matches** a prefix and a tail when its leading fields are the prefix, its trailing fields are the tail, and the two do not overlap.
Fields match whole and by byte equality, as {{moqt}} defines for a prefix already; the tail adds no new comparison.


# Introduction
{{moqt}} lets a subscriber ask for the namespaces under a prefix, and nothing else.
A subscriber that wants a subset of that prefix receives the whole thing and discards the rest, which costs a message per namespace it did not want and grows with the publisher's inventory rather than with the subscriber's interest.

Prefixes are not the natural shape of every interest.
A namespace's leading fields tend to say who owns the content and its trailing fields tend to say what the content is, so "every recording belonging to this tenant" is a prefix query while "every namespace of this kind, whoever owns it" is not expressible at all.
The second is what a subscriber wants whenever the discriminator is not at the front.

This extension adds the other end.
A SUBSCRIBE_NAMESPACE may carry a tail alongside its prefix, and the publisher answers with the namespaces matching both.
Either half may be empty; a tail alone selects by kind across every owner, and an empty tail is exactly the behavior of {{moqt}} today.


# Setup Negotiation
An endpoint indicates support with the following Setup Option ({{moqt}} Section 10.3), whose value is empty:

~~~
NAMESPACE_TAIL Setup Option {
  Option Key (vi64) = 0x40B61
  Option Value Length (vi64) = 0
}
~~~

An endpoint MUST NOT send the NAMESPACE_TAIL parameter to a peer that did not declare support.
A peer without this extension would ignore the parameter and answer the prefix alone, which is a strictly larger set: correct, but silently unfiltered, and a subscriber cannot tell that from a prefix whose contents genuinely all match.


# The NAMESPACE_TAIL Parameter {#tail}
NAMESPACE_TAIL is a Key-Value-Pair parameter ({{moqt}} Section 2.5) carried in SUBSCRIBE_NAMESPACE.

~~~
NAMESPACE_TAIL Parameter {
  Type (vi64) = 0x40B63
  Length (vi64)
  Track Namespace Tail (..)
}
~~~

The **Track Namespace Tail** is a sequence of Track Namespace tuple fields, encoded as a namespace is in {{moqt}}.
The message's Track Namespace Prefix is the head of the request and this parameter is its tail.

A publisher that has negotiated this extension MUST NOT send an advertisement whose namespace does not match both halves, and the subscriber's interest is otherwise unchanged: the request is answered, updated, and ended exactly as it is without the parameter.
The parameter MUST NOT appear more than once, and a receiver that sees it twice MUST treat the message as malformed per {{moqt}}'s Key-Value-Pair rules.

An empty tail matches every namespace and is equivalent to omitting the parameter.
A prefix and a tail that cannot both be satisfied by any namespace, because together they are longer than any namespace the publisher could serve, is not an error: it simply matches nothing.


# Filtering Without the Extension
A relay is frequently the only endpoint on a path that has this extension, because its downstream subscriber and its upstream publisher are separate deployments upgraded at separate times.

A relay MAY satisfy a request carrying a tail from an upstream subscription that does not, subscribing upstream with the prefix alone and discarding the advertisements that do not match the tail before forwarding.
This is REQUIRED behavior in the sense that there is no alternative when the upstream lacks the extension, and it is worth doing even when the upstream has it: several downstream requests sharing a prefix and differing only in tail can be served from one upstream subscription.

A relay SHOULD bound the distinct upstream subscriptions it opens on its subscribers' behalf.
Distinct prefixes correspond to distinct namespaces a subscriber must be authorized for, but distinct tails under one prefix all draw on the same upstream set, so a relay that opens one upstream subscription per requested tail multiplies its own upstream state at a subscriber's choosing.


# Security Considerations
The extension moves work from the subscriber to the publisher: the publisher evaluates a tail against its inventory where before the subscriber discarded what it did not want.
A publisher SHOULD treat a tail like any other request input and bound the work it will do per subscription, particularly where matching is not indexed.

Filtering is not access control.
A tail narrows what a subscriber is sent, never what it is entitled to, and a publisher MUST apply the same authorization to a filtered request as to the unfiltered prefix it narrows.


# IANA Considerations {#iana}

This document requests the following registrations, using high, distinctive values to avoid the low ranges reserved by {{moqt}} and the values sibling extensions already claim.

## MOQT Setup Options

One registration in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required:

| Value   | Name           | Reference     |
|:--------|:---------------|:--------------|
| 0x40B61 | NAMESPACE_TAIL | This Document |

## MOQT Message Parameters

One registration in the "MOQT Message Parameters" registry ({{moqt}} Section 15.7), whose policy is Specification Required:

| Value   | Name           | Carried In         | Reference     |
|:--------|:---------------|:-------------------|:--------------|
| 0x40B63 | NAMESPACE_TAIL | SUBSCRIBE_NAMESPACE | This Document |

Both keys are odd, so their values are length-prefixed: an empty value for the Setup Option, and a byte string for the parameter.
An even key would carry a varint instead, which cannot express an empty value, so the parity is not incidental.


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
