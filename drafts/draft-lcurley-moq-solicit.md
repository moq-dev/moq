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

This document defines an extension for MoQ Transport {{moqt}} that lets an endpoint declare that advertisements to it must be solicited first.
An endpoint that declares nothing receives unsolicited PUBLISH_NAMESPACE, which is what a peer unaware of this extension implicitly asks for.
An endpoint that will instead ask for what it wants says so once during setup, and is spared the advertisements it would otherwise have to ignore.
Because sending the option at all identifies an endpoint that implements this extension, the requirement is enforceable between two such endpoints rather than merely advisory.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}

An endpoint **advertises** a namespace by sending PUBLISH_NAMESPACE, or NAMESPACE in response to a SUBSCRIBE_NAMESPACE.
An advertisement is **solicited** when it is carried by a SUBSCRIBE_NAMESPACE the receiver sent, and **unsolicited** otherwise.

On a version of {{moqt}} that has NAMESPACE, the mechanism decides this and nothing else does: PUBLISH_NAMESPACE opens a request of its own and is always unsolicited, however the namespace relates to a prefix the receiver subscribed to.
That distinction is what makes the requirement observable from a single message, and so enforceable ({{enforcement}}).
Earlier versions have no NAMESPACE, so an endpoint answers a SUBSCRIBE_NAMESPACE with PUBLISH_NAMESPACE requests and the two are indistinguishable.


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


# Setup Negotiation

An endpoint declares whether it requires solicitation with the following Setup Option ({{moqt}} Section 10.3):

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


# Security Considerations

A declaration only ever reduces what its sender receives, so an attacker who forges one can silence advertisements to the endpoint it impersonates, or add an option to an endpoint that never sent one so its own advertisements are treated as a violation and its session closed.
Both are already available to anyone who can interfere with SETUP, which {{moqt}} assumes is protected by the underlying transport.

A declaration says nothing about authorization.
An endpoint that declares no requirements is not thereby entitled to any advertisement, and a peer MUST apply the same authorization to what it advertises as it would without this extension.
In particular, a relay MUST NOT treat an absent SOLICIT option as permission to advertise namespaces the peer is not authorized to learn about.

The declaration reveals a little about an endpoint's intent, roughly whether it intends to ask for what it wants.
An endpoint that considers this sensitive can simply declare nothing, which costs it only the messages it would have avoided.


# IANA Considerations

This document requests the following registration.
A high, distinctive value is requested to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions.

## MOQT Setup Options

This document requests one registration in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required.

| Value   | Name    | Reference     |
|:--------|:--------|:--------------|
| 0x40B5A | SOLICIT | This Document |

The Key-Value-Pair parity is load-bearing: SOLICIT is even, so its value is a bare varint.
This document defines only the values 0 and 1; a later extension that needs to say something else registers its own option rather than overloading this one.


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
