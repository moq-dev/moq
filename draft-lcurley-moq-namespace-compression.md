---
title: "MoQ Namespace Compression Extension"
abbrev: "moq-namespace-compression"
category: info

docname: draft-lcurley-moq-namespace-compression-latest
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
  RFC1951:
  RFC7692:

informative:

--- abstract

This document defines an extension for MoQ Transport {{moqt}} that compresses namespace advertisements sent in response to SUBSCRIBE_NAMESPACE.
The extension defines a namespace suffix compression negotiation and a DEFLATE compression algorithm for the Track Namespace Suffix carried by NAMESPACE and NAMESPACE_DONE messages, retaining compression context across messages on the same namespace subscription response stream and flushing after each message.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}


# Introduction

MoQ Transport {{moqt}} lets a subscriber discover published namespaces by sending SUBSCRIBE_NAMESPACE.
The publisher responds with NAMESPACE messages for matching namespaces and NAMESPACE_DONE messages when a matching namespace is no longer available.

In applications with many live publishers, these namespace suffixes often share repeated structure, such as room identifiers, participant identifiers, or media role names.
This extension compresses only the Track Namespace Suffix field of NAMESPACE and NAMESPACE_DONE messages.
The rest of the control message framing remains unchanged, allowing each namespace change to remain an independent protocol event.

The compression model intentionally mirrors WebSocket per-message compression {{RFC7692}}: compressed data is flushed at each message boundary so the receiver can process each message immediately, and the fixed sync-flush marker is removed from the bytes carried on the wire.


# Setup Negotiation

The Namespace Compression extension is negotiated during the SETUP exchange as defined in {{moqt}} Section 10.3.
An endpoint indicates support by including the following Setup Option:

~~~
NAMESPACE_COMPRESSION Setup Option {
  Option Key (vi64) = 0x40B59
  Option Value Length (vi64)
  Namespace Compression Algorithm (vi64)
}
~~~

**Namespace Compression Algorithm**:
The namespace suffix compression algorithm supported by the endpoint.

- `deflate` (0): DEFLATE compression as defined in [Compressed Suffix Encoding](#compressed-suffix-encoding).

The Option Value MUST contain exactly one Namespace Compression Algorithm.
An endpoint that receives an unknown Namespace Compression Algorithm MUST close the session with PROTOCOL_VIOLATION.

If both endpoints include NAMESPACE_COMPRESSION with the same Namespace Compression Algorithm, then namespace suffix compression is negotiated for the session.
When namespace suffix compression is negotiated, every NAMESPACE and NAMESPACE_DONE message sent in response to SUBSCRIBE_NAMESPACE on the session MUST encode its Track Namespace Suffix using the negotiated algorithm.
If either endpoint omits NAMESPACE_COMPRESSION, namespace suffix compression is not negotiated and NAMESPACE and NAMESPACE_DONE use the encoding defined by {{moqt}}.

The extension applies to a single hop and is negotiated independently for each session.
A relay MUST NOT assume support on one session implies support on another.


# Compressed Namespace Suffixes

This extension does not define new message types and does not otherwise replace the NAMESPACE or NAMESPACE_DONE message definitions in {{moqt}}.
Instead, when namespace suffix compression is negotiated, only the Track Namespace Suffix field in each NAMESPACE and NAMESPACE_DONE message is compressed.
All other fields and any future fields in those messages retain the encoding defined by {{moqt}} or by the extension that defines them.

**Track Namespace Suffix**:
When namespace suffix compression is negotiated, this field carries the compressed form of the Track Namespace Suffix value.
The uncompressed bytes are exactly the Track Namespace Suffix encoding defined by {{moqt}}, excluding the field's own length prefix if the base encoding supplies one.

After decompression, the receiver parses the resulting bytes as a Track Namespace Suffix.
If decompression fails, or if the decompressed bytes are not a valid Track Namespace Suffix, the receiver MUST close the session with PROTOCOL_VIOLATION.


# Compressed Suffix Encoding
{: #compressed-suffix-encoding}

The `deflate` Namespace Compression Algorithm uses the DEFLATE format {{RFC1951}} with one compression context per SUBSCRIBE_NAMESPACE response stream.
The context is initialized when the publisher accepts the SUBSCRIBE_NAMESPACE request and is retained until the response stream is closed.

For each NAMESPACE or NAMESPACE_DONE message, the publisher feeds the uncompressed Track Namespace Suffix bytes into that stream's compression context and then performs the equivalent of `Z_SYNC_FLUSH`.
A sync flush produces output ending with the fixed four-byte marker `00 00 ff ff`; the publisher MUST remove this marker before writing the compressed bytes into the Track Namespace Suffix field.

The subscriber uses one decompression context for the same response stream.
Before inflating each compressed Track Namespace Suffix, the subscriber appends the four bytes `00 00 ff ff` to reconstruct the sync-flushed DEFLATE stream.
Because the publisher flushes after every NAMESPACE or NAMESPACE_DONE message, the subscriber can decompress and process each namespace update as soon as that message is received.
The flush does not reset the compression context, so repeated namespace components in later messages can reference bytes from earlier messages on the same response stream.


# Security Considerations

This extension adds stateful decompression to namespace discovery.
Implementations SHOULD bound the amount of memory and CPU used for each compression context and SHOULD apply the same limits to decompressed namespace suffixes that they apply to uncompressed suffixes.

Compression can reveal information through compressed sizes when an attacker can influence namespace names and observe response sizes.
Applications that consider namespace names sensitive SHOULD avoid enabling this extension across trust boundaries, or partition namespace subscriptions so attacker-controlled and secret namespace components do not share a compression context.

Malformed compressed data is a protocol violation.
Repeated decompression failures can be used as a denial-of-service signal and MAY be rate limited or logged by implementations.


# IANA Considerations

This document requests the following registrations.
High, distinctive values are requested to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions; they also avoid the greasing pattern (`0x7f * N + 0x9D`).

## MOQT Setup Options

This document requests a registration in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required.

| Value   | Name              | Reference     |
|:--------|:------------------|:--------------|
| 0x40B59 | NAMESPACE_COMPRESSION | This Document |

## MOQT Namespace Compression Algorithms

This document requests a new "MOQT Namespace Compression Algorithms" registry, whose policy is Specification Required.

| Value | Name    | Reference     |
|:------|:--------|:--------------|
| 0     | DEFLATE | This Document |


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of AI tools.
