---
title: "MoQ Solicit Extension"
abbrev: "moq-solicit"
category: info

docname: draft-lcurley-moq-solicit-latest
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

This document defines an extension for MoQ Transport {{moqt}} that settles two things the base protocol leaves an endpoint guessing about: which of the two announce mechanisms its peer expects, and where the answer to a SUBSCRIBE_NAMESPACE ends.
An endpoint that declares nothing receives unsolicited PUBLISH_NAMESPACE, which is what a peer unaware of this extension implicitly asks for.
An endpoint that will instead ask for what it wants says so once during setup, and is spared the advertisements it would otherwise have to ignore.
It can also ask each SUBSCRIBE_NAMESPACE response to report how many NAMESPACE messages make up the set the publisher already had, so it can tell an empty prefix from one it has not been told about yet.
The two travel together: the count answers for the stream the advertisements arrive on, which is the stream solicitation puts them on.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}

An endpoint **advertises** a namespace by sending PUBLISH_NAMESPACE, or NAMESPACE in response to a SUBSCRIBE_NAMESPACE.
An advertisement is **solicited** when it is carried by a SUBSCRIBE_NAMESPACE the receiver sent, and **unsolicited** otherwise.

On a version of {{moqt}} that has NAMESPACE, the mechanism decides this and nothing else does: PUBLISH_NAMESPACE opens a request of its own and is always unsolicited, however the namespace relates to a prefix the receiver subscribed to.
That distinction is what makes the requirement observable from a single message, and so enforceable ({{enforcement}}).
Earlier versions have no NAMESPACE, so an endpoint answers a SUBSCRIBE_NAMESPACE with PUBLISH_NAMESPACE requests and the two are indistinguishable.

The **initial set** is the namespaces a publisher holds under a prefix at the moment it answers a SUBSCRIBE_NAMESPACE for it.
It is a snapshot of one publisher, not of the network: a relay's initial set is what that relay holds, which may be less than its own upstream will eventually tell it.


# Introduction
{{moqt}} has two ways to learn that a namespace exists.
A publisher advertises one with PUBLISH_NAMESPACE, and a subscriber asks for the set under a prefix with SUBSCRIBE_NAMESPACE, which is answered with NAMESPACE.
Both are optional, and nothing on the wire says which the peer expects.

The result is that an endpoint has to guess, and both guesses are wrong somewhere:

- Withhold PUBLISH_NAMESPACE until asked, and a publisher connected to a relay that never asks stays silent forever. The session looks healthy and no content is ever offered.
- Send it unasked, and an endpoint that only publishes is told about every namespace its peer knows, which is a set it has no use for and, on a relay, a set as large as the network.

Implementations resolve this out of band today, by configuring each endpoint with what the peer will do.
That works only as long as the configuration matches reality, and it makes the same software behave differently depending on a deployment flag rather than on anything in the protocol.

This extension replaces the guess with a declaration.
An endpoint states in its SETUP that it requires advertisements to be solicited, and the peer honors it.
The default, declaring nothing, is to advertise unasked, so an endpoint that has never heard of this extension keeps working exactly as it does today.

This is deliberately not a role: it says what an endpoint expects delivered to it, not what it is.
An endpoint that sends SUBSCRIBE_NAMESPACE for everything it cares about declares it on every session, whether it publishes, subscribes, or relays.

Settling the mechanism exposes a second gap, which is why this document also settles it.
Once advertisements arrive as NAMESPACE messages on the stream a subscriber opened, the ones describing what the publisher already had and the ones announcing what appeared a moment later are the same message, arriving in the same order they were learned, with nothing separating them.
A subscriber can therefore observe that a namespace is present, but never that it is absent.
Anything that needs the negative answer (resolving a name before falling back to another source, deciding whether to fetch on demand, listing what a prefix offers) has only a timeout to work with, and a timeout is wrong in both directions: too short and a slow publisher looks empty, too long and every miss pays for it.
The NAMESPACE_COUNT parameter ({{count}}) puts the size of the initial set in the response, so the subscriber counts messages instead of waiting.


# Setup Negotiation

This document defines two Setup Options ({{moqt}} Section 10.3), negotiated independently.

## Requiring Solicitation {#solicit-option}

An endpoint declares whether it requires solicitation with the following Setup Option:

~~~
SOLICIT Setup Option {
  Option Key (vi64) = 0x40B5A
  Option Value (vi64) = 0 or 1
}
~~~

A value of 1 means advertisements to the sender MUST be solicited: it sends SUBSCRIBE_NAMESPACE for what it wants.
A value of 0 means it has no requirement, so an advertisement may be sent freely.

The option is OPTIONAL, and omitting it asks for the same treatment as a value of 0.
The two are not equivalent, because sending either value also identifies the sender as implementing this extension ({{enforcement}}), while omitting it says nothing at all.
An endpoint that implements this extension and has no requirement SHOULD therefore send 0 rather than omit the option.

A receiver MUST treat any non-zero value as 1.
The two directions are independent: each endpoint declares its own, and the two need not match.

Unlike an extension that changes an encoding, this one needs no negotiation handshake: a declaration only ever asks the peer to send *less*, so a peer that ignores it is merely as talkative as one that never saw it.

## Asking for the Count {#count-option}

Unknown Message Parameters close the session ({{moqt}} Section 10.2), so unlike the option above this one does have to be negotiated: the count is only sendable to an endpoint that asked for it.

~~~
NAMESPACE_COUNT Setup Option {
  Option Key (vi64) = 0x40B5C
  Option Value (vi64) = 0 or 1
}
~~~

A value of 1 asks for the NAMESPACE_COUNT parameter ({{count}}) on every SUBSCRIBE_NAMESPACE response the sender receives.
A value of 0, and omitting the option, ask for nothing; the two are equivalent here, since nothing in this mechanism turns on whether a peer that wants no count implements it.

A receiver MUST treat any non-zero value as 1, and MUST NOT send the parameter to a peer that did not declare 1.

The two options are separate code points rather than one, even though an endpoint usually declares both.
An endpoint that implements only the option above declares SOLICIT and has never heard of the parameter, so reading its declaration as a request for one would close its session on the first response.


# Requiring Solicitation {#announce}

An endpoint that declared 1 will solicit the namespaces it wants with SUBSCRIBE_NAMESPACE, or wants none at all.

A peer that receives this declaration and implements this extension MUST NOT send an unsolicited PUBLISH_NAMESPACE for the remainder of the session; one that does not implement it cannot be bound by it and is covered by {{enforcement}}.
It continues to answer SUBSCRIBE_NAMESPACE with NAMESPACE as usual; only the unsolicited half is withheld.

This is a MUST NOT rather than a SHOULD NOT because the receiver enforces it. A SHOULD NOT that a receiver may close the session over is not a permission an endpoint can actually exercise.

A relay is the expected user of this declaration, as is any endpoint that asks for what it wants.
So is an endpoint that only publishes, which cannot subscribe to anything and therefore has no use for an advertisement of any kind.

An endpoint SHOULD NOT advertise the same namespace both ways on one session.
Whichever arrives second replaces the source the first attached, which at best wastes a stream and at worst leaves the receiver holding two independent advertisements it must reconcile.
Because this declaration decides which of the two an endpoint uses, honoring it also settles that question for the whole session.


# The Initial Set {#count}

~~~
NAMESPACE_COUNT Parameter {
  Type (vi64) = 0x40B5E
  Value (vi64)
}
~~~

The value is the number of NAMESPACE messages that carry the initial set, and they are the first NAMESPACE messages to follow the response.
0 is valid and means the publisher has nothing under the prefix, so the set is complete when the response arrives.

The parameter is carried in SUBSCRIBE_NAMESPACE_OK ({{moqt}} Section 10.5) and nowhere else.
A publisher MUST include it on every SUBSCRIBE_NAMESPACE_OK it sends to a peer that declared 1, except where {{unsolicited-count}} says to omit it, and a receiver ignores it on a REQUEST_OK answering anything else.
Requiring it on every response it does apply to is what makes the count usable: a parameter a publisher may omit at will says nothing when it is missing.

A publisher counts only the namespaces it will actually send, after applying whatever authorization and filtering it applies to the stream.
A count larger than that leaves the subscriber waiting for messages that are never coming.

## Sending the Initial Set

A publisher MUST send every counted message before any other NAMESPACE or NAMESPACE_DONE on that stream.

A namespace that goes away after being counted is still sent, followed by a NAMESPACE_DONE.
The count describes the stream, not what remains true, and a publisher that quietly dropped one would leave the set incomplete forever.

A publisher never has to wait to answer: an initial set is a snapshot of what it holds when it responds, so whatever it has not learned yet is simply not in it and arrives later as a live update.
A relay MAY nonetheless defer its response until its own upstream set is complete, which is what makes a chain of relays report one prefix consistently, and SHOULD bound how long it will wait so one slow upstream does not stall every subscriber below it.

## Advertising Unsolicited {#unsolicited-count}

A count answers for one stream, so it only answers the subscriber's question when that stream is where the advertisements are.

A publisher that advertises to this peer with unsolicited PUBLISH_NAMESPACE therefore MUST omit the parameter, even though the peer asked for it.
Its SUBSCRIBE_NAMESPACE response carries nothing (the peer has already been told, and {{announce}} says not to say it twice), so the only count it could report is 0, which reads as "this prefix is empty" while the advertisements are still in flight on their own streams.
Omitting it says what is true instead: this stream is not marking a boundary.

This is why the two options travel together.
An endpoint that declares both gets a complete answer, because solicitation is what moves the advertisements onto the stream the count describes.
An endpoint that asks only for the count gets one from a publisher that answers on request anyway, and nothing from one that does not.

## Receiving the Initial Set {#absent}

A subscriber MAY withhold the initial set from the application until the last counted NAMESPACE arrives and then deliver it as a batch.
Messages after that are live updates and SHOULD be reported as they arrive.

Once the set is complete, a namespace that was not in it is one the publisher does not have.
A subscriber MAY act on that, and MUST NOT read it as the namespace being unavailable anywhere else.

A subscriber MUST NOT wait indefinitely for a set to complete.
A publisher that counts more than it sends would otherwise hold it forever ({{security}}), and a stream close or reset is the only end the protocol itself provides: {{moqt}} already treats it as withdrawing every live namespace, which leaves nothing outstanding to count.

A subscriber that declared 1 and receives a response without the parameter has no boundary for that prefix, and MUST NOT synthesize one.
A set declared complete early is worse than one never declared complete at all.

SUBSCRIBE_TRACKS ({{moqt}} Section 10.19) is answered with PUBLISH messages on separate streams rather than NAMESPACE messages on the response stream, and is out of scope for this document.
So is a version of {{moqt}} with no NAMESPACE message, where the response set is not carried on the response stream and there is nothing to count.


# Enforcement {#enforcement}

An endpoint that declared 1 and then receives an unsolicited PUBLISH_NAMESPACE MUST close the session as a protocol violation if the sender declared either value, and MUST tolerate the message otherwise.

An endpoint that sent the option implements this extension, so it read the receiver's declaration and ignored it.
It also cannot have advertised before reading that declaration: the receiver's SETUP is what says whether advertising unasked is permitted at all, so an endpoint MUST have processed it before sending its first advertisement.
Neither a race nor a partial implementation explains the message, which leaves a bug, and one that both sides would otherwise never see.

An endpoint that omitted the option gets the opposite treatment for the same reason: it never saw the declaration, so announcing is exactly what it should do, and closing the session over it would turn this extension into a new way for conforming implementations to fail to interoperate.

Versions of {{moqt}} without NAMESPACE are exempt, because a PUBLISH_NAMESPACE there is also how an endpoint answers a SUBSCRIBE_NAMESPACE and the message alone does not say which it is.
Everywhere else the mechanism is the whole test: a receiver MUST NOT treat an advertisement as solicited merely because the namespace falls under a prefix it subscribed to, or an endpoint that subscribes to every prefix it can reach would find the requirement unenforceable against anyone.

There is no counterpart for SUBSCRIBE_NAMESPACE, enforceable or otherwise.
An endpoint with nothing to advertise answers one with an empty set, which costs a single stream, while waiting on the peer's SETUP to learn whether the question is worth asking costs a round trip on every session.
Asking unconditionally is therefore the cheaper behavior, and it is what an endpoint SHOULD do.


# Security Considerations {#security}

A declaration only ever reduces what its sender receives, so an attacker who forges one can silence advertisements to the endpoint it impersonates, or add an option to an endpoint that never sent one so its own advertisements are treated as a violation and its session closed.
Both are already available to anyone who can interfere with SETUP, which {{moqt}} assumes is protected by the underlying transport.

A declaration says nothing about authorization.
An endpoint that declares no requirements is not thereby entitled to any advertisement, and a peer MUST apply the same authorization to what it advertises as it would without this extension.
In particular, a relay MUST NOT treat an absent SOLICIT option as permission to advertise namespaces the peer is not authorized to learn about.

The declaration reveals a little about an endpoint's intent, roughly whether it intends to ask for what it wants.
An endpoint that considers this sensitive can simply declare nothing, which costs it only the messages it would have avoided.

A count is a claim, and a subscriber can only ever be harmed by trusting it.
Counting more than is sent holds the subscriber waiting, which is why the wait is bounded ({{absent}}); counting less makes it treat a real namespace as a live update, which costs it nothing.
A subscriber MUST NOT allocate memory in proportion to a count, since it is one varint and can be arbitrarily large.

A count reveals the size of the set at a prefix, which the messages that follow reveal anyway, and only the part of it the subscriber is authorized to see.
It says nothing about namespaces the publisher withheld, and a publisher MUST NOT count them.


# IANA Considerations

This document requests the following registration.
A high, distinctive value is requested to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions.

## MOQT Setup Options

This document requests two registrations in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required.

| Value   | Name            | Reference     |
|:--------|:----------------|:--------------|
| 0x40B5A | SOLICIT         | This Document |
| 0x40B5C | NAMESPACE_COUNT | This Document |

## MOQT Message Parameters

This document requests one registration in the "MOQT Message Parameters" registry ({{moqt}} Section 15.7).

| Value   | Name            | Carried In             | Reference     |
|:--------|:----------------|:-----------------------|:--------------|
| 0x40B5E | NAMESPACE_COUNT | SUBSCRIBE_NAMESPACE_OK | This Document |

The Key-Value-Pair parity is load-bearing: every value here is even, so each carries a bare varint.
Each option defines only the values 0 and 1; a later extension that needs to say something else registers its own rather than overloading one of these.


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
