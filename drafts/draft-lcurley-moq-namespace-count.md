---
title: "MoQ Namespace Count Extension"
abbrev: "moq-namespace-count"
category: info

docname: draft-lcurley-moq-namespace-count-latest
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

This document defines an extension for MoQ Transport {{moqt}} that reports how many NAMESPACE messages answer a SUBSCRIBE_NAMESPACE.
The count rides in the response, so the arrival of the last one marks the end of the set the publisher already had, and everything after it is a live update.
A subscriber can then tell "the publisher has nothing under this prefix" from "the publisher has not told me yet", which the base protocol leaves indistinguishable.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}

The **initial set** is the namespaces a publisher holds under a prefix at the moment it answers a SUBSCRIBE_NAMESPACE for it.
It is a snapshot of one publisher, not of the network: a relay's initial set is what that relay holds, which may be less than its own upstream will eventually tell it.


# Introduction

{{moqt}} answers a SUBSCRIBE_NAMESPACE with a REQUEST_OK, then sends a NAMESPACE for each matching namespace on the response stream ({{moqt}} Section 10.18).
The ones describing what the publisher already had and the ones announcing what appeared a moment later are the same message, arriving on the same stream, in the same order they were learned.
Nothing separates them.

A subscriber can therefore observe that a namespace is present, but never that it is absent.
Anything that needs the negative answer (resolving a name before falling back to another source, deciding whether to fetch on demand, listing what a prefix offers) has only a timeout to work with, and a timeout is wrong in both directions: too short and a slow publisher looks empty, too long and every miss pays for it.

This extension puts the size of the initial set in the response.
The subscriber counts NAMESPACE messages, and the set is complete when it has that many.

The count is a statement about the response stream, not a promise about the network.
It says what the publisher held when it answered, which is exactly the question a subscriber can act on: content the publisher does not have, it cannot serve.


# Setup Negotiation

Unknown Message Parameters close the session ({{moqt}} Section 10.2), so the count is only sendable to an endpoint that asked for it.
An endpoint asks with the following Setup Option ({{moqt}} Section 10.3.1):

~~~
NAMESPACE_COUNT Setup Option {
  Option Key (vi64) = 0x40B5C
  Option Value (vi64) = 0 or 1
}
~~~

A value of 1 asks for the NAMESPACE_COUNT parameter ({{count}}) on every SUBSCRIBE_NAMESPACE response the sender receives.
A value of 0, and omitting the option, ask for nothing; the two are equivalent, since this extension gives an endpoint no reason to distinguish an implementation that declined from one that never heard of it.

A receiver MUST treat any non-zero value as 1.
The two directions are independent: an endpoint that subscribes to namespaces declares 1 whether or not it also publishes any, and its peer answers for its own direction.

An endpoint MUST NOT send the parameter to a peer that did not declare 1.
There is nothing else to negotiate, because the count is the entire extension: an endpoint that declared 1 and receives a response without the parameter is talking to a peer that ignored the option, and is no worse off than it is today ({{absent}}).


# The NAMESPACE_COUNT Parameter {#count}

~~~
NAMESPACE_COUNT Parameter {
  Type (vi64) = 0x40B5E
  Value (vi64)
}
~~~

The value is the number of NAMESPACE messages that carry the initial set.
0 is valid and means the publisher has nothing under the prefix, so the set is complete when the response arrives.

The parameter is carried on the REQUEST_OK ({{moqt}} Section 10.5) that opens a namespace response set, and nowhere else.
That is SUBSCRIBE_NAMESPACE_OK, and the REQUEST_UPDATE_OK accepting a change of Track Namespace Prefix ({{moqt}} Section 10.9.2), which starts a fresh set on the same stream and needs its own boundary for the same reason.
A publisher MUST include it on every such response it sends to a peer that declared 1, and MUST NOT put it on a REQUEST_OK answering anything else, where a receiver ignores it.
Requiring it on every one of them is what makes the count usable: a parameter a publisher may omit says nothing when it is missing.

A publisher counts only the namespaces it will actually send, after applying whatever authorization and filtering it applies to the stream.
A count larger than that leaves the subscriber waiting for messages that are never coming.


# Sending the Initial Set

The counted messages are the first NAMESPACE messages that follow the response.
A publisher MUST send all of them before any other NAMESPACE or NAMESPACE_DONE on that stream.

A namespace that goes away after being counted is still sent, followed by a NAMESPACE_DONE.
The count describes the stream, not what remains true, and a publisher that quietly dropped one would leave the set incomplete forever.

A publisher never has to wait to answer: an initial set is a snapshot of what it holds when it responds, so whatever it has not learned yet is simply not in it and arrives later as a live update.
A relay MAY nonetheless defer its response until its own upstream set is complete, which is what makes a chain of relays report one prefix consistently, and SHOULD bound how long it will wait so one slow upstream does not stall every subscriber below it.


# Receiving the Initial Set {#absent}

A subscriber MAY withhold the initial set from the application until the last counted NAMESPACE arrives and then deliver it as a batch.
Messages after that are live updates and SHOULD be reported as they arrive.

Once the set is complete, a namespace that was not in it is one the publisher does not have.
A subscriber MAY act on that, and MUST NOT read it as the namespace being unavailable anywhere else.

A subscriber MUST NOT wait indefinitely for a set to complete.
A publisher that counts more than it sends would otherwise hold the subscriber forever ({{security}}).
A stream close or reset also ends the wait: {{moqt}} already treats it as withdrawing every live namespace, which leaves nothing outstanding to count.

A subscriber that declared 1 and receives a response without the parameter has learned that its peer does not implement this extension, and is left with the base protocol's behavior.
It has no boundary for that prefix and MUST NOT synthesize one, since a set it declares complete early is worse than one it never declares complete at all.

SUBSCRIBE_TRACKS ({{moqt}} Section 10.19) is answered with PUBLISH messages on separate streams rather than NAMESPACE messages on the response stream, and is out of scope for this document.
So is a version of {{moqt}} with no NAMESPACE message, where the response set is not carried on the response stream and there is nothing to count.


# Security Considerations {#security}

The count is a claim, and a subscriber can only ever be harmed by trusting it.
Counting more than is sent holds the subscriber waiting, which is why the wait is bounded ({{absent}}); counting less makes it treat a real namespace as a live update, which costs it nothing.
A subscriber MUST NOT allocate memory in proportion to the count, since it is one varint and can be arbitrarily large.

The value reveals the size of the set at a prefix, which the messages that follow reveal anyway, and only the part of it the subscriber is authorized to see.
It says nothing about namespaces the publisher withheld, and a publisher MUST NOT count them.


# IANA Considerations

This document requests the following registrations.
High, distinctive values are requested to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions.

## MOQT Setup Options

This document requests one registration in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required.

| Value   | Name            | Reference     |
|:--------|:----------------|:--------------|
| 0x40B5C | NAMESPACE_COUNT | This Document |

## MOQT Message Parameters

This document requests one registration in the "MOQT Message Parameters" registry ({{moqt}} Section 15.7).

| Value   | Name            | Carried In                                  | Reference     |
|:--------|:----------------|:--------------------------------------------|:--------------|
| 0x40B5E | NAMESPACE_COUNT | SUBSCRIBE_NAMESPACE_OK, REQUEST_UPDATE_OK   | This Document |

Both values are even, so each carries a bare varint on the versions of {{moqt}} that encode these as Key-Value-Pairs.
The Setup Option and the Message Parameter share a name because they are one mechanism: the option asks for the parameter, and the parameter is the answer.


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
