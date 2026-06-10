---
title: "MoQ Payload Compression Extension"
abbrev: "moq-compression"
category: info

docname: draft-lcurley-moq-compression-latest
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

informative:

--- abstract

This document defines a payload compression extension for MoQ Transport {{moqt}}.
A track-level Compression property lets the original publisher signal that a track's object payloads are worth compressing, and with which algorithm.
Compression is then applied independently on each hop: a payload is compressed only on a hop that has negotiated the extension and whose receiver supports the algorithm, and is sent verbatim otherwise.
Each object is compressed independently so objects remain individually decodable, and the decompressed bytes — the actual object — are unchanged end to end.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}


# Introduction
{{moqt}} makes the original publisher "solely responsible for the content of the object payload ... including the underlying encoding, compression, any end-to-end encryption, or authentication" ({{moqt}} Section 2.1).
For media this is the right layering: already-compressed codecs (H.264, Opus, AV1) gain nothing from a second compression pass.

But MoQ also carries non-media tracks — JSON, text, telemetry, captions, uncompressed binary structures — where the payloads are highly compressible and where end-to-end encryption is often not in use.
For these tracks there is no standard, transport-visible way to compress payloads, so each application reinvents it, and relays cannot help.

Like HTTP Transfer-Encoding, the on-wire compression is a hop-by-hop optimization: it does not conceptually change the object payload — the decompressed bytes *are* the object — it only changes how those bytes are carried over a single hop.
What this extension adds on top is an end-to-end *signal*: a track property by which the original publisher marks the content as worth compressing and names the algorithm. The signal travels end-to-end; the compression happens per hop.

- **Publisher signals, hops apply**: the COMPRESSION track property is set by the original publisher and carried end-to-end, but a payload is only compressed on a hop that negotiated the extension and whose receiver supports the algorithm. Where the extension is not negotiated, the same payload travels verbatim.
- **Per object, independently**: each object payload is an independent compressed stream with no shared dictionary or state between objects. This keeps every object individually decodable and avoids head-of-line decoding within a group.


# Setup Negotiation
The Payload Compression extension is negotiated during the SETUP exchange as defined in {{moqt}} Section 10.3.
Unlike a purely additive property, compression MUST be negotiated: a receiver that does not understand the algorithm would otherwise pass the compressed bytes to the application as if they were plaintext.

Each endpoint advertises the algorithms it can decompress by including the following Setup Option:

~~~
COMPRESSION Setup Option {
  Option Key (vi64) = 0xC03DE
  Option Value Length (vi64)
  Algorithm (vi64) ...
}
~~~

**Algorithm**:
One or more Algorithm identifiers (see [Compression Algorithms](#compression-algorithms)) that the sender can decompress, each a varint, filling the Option Value.
The identifier `none` (0) MUST NOT be listed (it requires no negotiation).

A sender MUST NOT compress with an algorithm the receiver did not advertise in its SETUP.
This makes the on-wire state unambiguous on every hop without any per-object signaling: a receiver decompresses a track's payloads **if and only if** the COMPRESSION track property is present and the receiver advertised that algorithm in its own SETUP. In every other case — the property absent, the extension not negotiated, or the algorithm not advertised by the receiver — the sender was not permitted to compress, so the receiver treats the payloads as verbatim.


# COMPRESSION Track Property
The COMPRESSION property is the original publisher's signal that a track's object payloads are worth compressing, and which algorithm to use.
It is a track-level Key-Value-Pair carried with the track's properties (see {{moqt}} Section 2.5 and Section 12), set by the original publisher and forwarded unchanged by relays.
Because the value is a single integer, COMPRESSION uses an even Type so the value is a bare varint:

~~~
COMPRESSION Track Property {
  Type (vi64) = 0xC03D0
  Value (vi64)  ; Algorithm identifier
}
~~~

**Value**:
The Algorithm identifier the publisher recommends for this track's payloads.
The absence of the property, or a value of `none` (0), means the track is not marked for compression and its payloads are always transmitted verbatim.

The property is fixed for the lifetime of the track and MUST NOT change.
A relay MUST forward it unchanged on every hop, including a hop that has not negotiated the extension: there it is simply an ignored unknown Key-Value-Pair, but forwarding it lets a further-downstream hop that does negotiate the extension still act on the publisher's signal.

Compression is enabled only by the combination of this track property and the extension being negotiated on a hop.
A publisher MUST NOT compress object payloads on a track that does not carry the COMPRESSION property, and there is no way to enable compression on a per-object basis: the property governs the whole track, and on a compressing hop every non-empty payload is compressed.

Whether payloads are actually compressed is decided per hop:

- On a hop where the extension is negotiated and the receiver advertised the property's algorithm, every non-empty object payload MUST be compressed with that algorithm, and the receiver decompresses it.
- On any other hop — the extension not negotiated, or the receiver did not advertise that algorithm — payloads are sent verbatim. The receiver either never sees the property (an ignored unknown Key-Value-Pair) or sees it but knows the sender was not permitted to compress for it, so it treats the payloads as verbatim either way.

Compression applies to the object payload only; object properties and message framing are never compressed.
An empty payload (size 0) MUST NOT be compressed and remains empty on the wire.

A publisher SHOULD set COMPRESSION only for payload types that benefit from it.
Already-compressed media SHOULD omit it (or use `none`).


# Compression Algorithms {#compression-algorithms}
This document defines the following algorithms.
Further algorithms MAY be registered (see [IANA Considerations](#iana-considerations)).

| ID | Name    | Description                                              |
|---:|:--------|:--------------------------------------------------------|
| 0  | none    | Payloads are transmitted verbatim. The default.        |
| 1  | deflate | Raw DEFLATE {{RFC1951}}, with no zlib or gzip framing.  |

For `deflate`, each object payload is an independent raw DEFLATE stream.
There is no shared dictionary or state between objects, so each object decompresses on its own.


# Relay Behavior
A relay forwards the COMPRESSION track property unchanged — it is the publisher's end-to-end signal — and applies compression independently on each hop.

On its upstream subscription, the relay receives payloads compressed if and only if that hop compressed them (the extension negotiated and the relay advertised the algorithm); it decompresses them as needed.
On each downstream subscription the relay serves, it compresses payloads with the track's algorithm when that downstream negotiated the extension and advertised the algorithm, and sends them verbatim otherwise.

Compression is thus driven by the publisher's track property, not by the relay: a relay does not compress a track the publisher did not mark.
In every case the decompressed bytes delivered to the application MUST be identical to what the origin published.

A relay or generic library MUST NOT inspect or modify the decompressed contents unless otherwise negotiated; only recompression that preserves the decompressed bytes exactly is permitted.


# Security Considerations
Compressing data that mixes attacker-controlled and secret content in the same object can leak the secret through compressed size, as in the CRIME and BREACH attacks.
A publisher MUST NOT set COMPRESSION on a track whose object payloads combine secret material with attacker-influenced material.
Because compression here is per-object with no cross-object dictionary, the exposure is bounded to within a single object, but it is not eliminated.

A malicious sender could emit a small compressed payload that decompresses to a very large buffer (a "decompression bomb").
A receiver MUST bound the size of a decompressed object payload. If the bound is exceeded it MUST reset the affected Subscribe/Fetch stream (rather than allocate unbounded memory) and MAY close the session with a PROTOCOL_VIOLATION if it considers the peer abusive; the reset is stream-scoped so a single bad object does not tear down unrelated subscriptions.

Compression is orthogonal to {{moqt}} end-to-end encryption: an encrypted payload is effectively incompressible, so a publisher using end-to-end encryption SHOULD omit COMPRESSION (or use `none`).


# IANA Considerations

This document requests the following registrations.
High, distinctive values are requested to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions; they also avoid the greasing pattern (`0x7f * N + 0x9D`).
The parameter Type is even so that its value is a bare varint with no length prefix (see {{moqt}} Section 2.5).

## MOQT Setup Options

This document requests a registration in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required.

| Value   | Name        | Reference     |
|:--------|:------------|:--------------|
| 0xC03DE | COMPRESSION | This Document |

## MOQT Properties

This document requests a registration in the "MOQT Properties" registry ({{moqt}} Section 15.8), used for object and track properties.

| Value   | Name        | Scope | Reference     |
|:--------|:------------|:------|:--------------|
| 0xC03D0 | COMPRESSION | Track | This Document |

## MOQT Compression Algorithms

This document requests a new "MOQT Compression Algorithms" registry, with a registration policy of Specification Required.
The initial contents are:

| ID | Name    | Reference     |
|---:|:--------|:--------------|
| 0  | none    | This Document |
| 1  | deflate | This Document |


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
