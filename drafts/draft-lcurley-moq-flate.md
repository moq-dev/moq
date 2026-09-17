---
title: "MoQ DEFLATE Extension"
abbrev: "moq-flate"
category: info

docname: draft-lcurley-moq-flate-latest
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
  moql: I-D.lcurley-moq-lite
  hang: I-D.lcurley-moq-hang

--- abstract

This document specifies how a MoQ Transport {{moqt}} track carries DEFLATE-compressed payloads.
Each unit of ordered and reliable delivery, a subgroup, is one raw DEFLATE stream sync-flushed at each object boundary: every object stays self-delimited while later objects compress against the earlier ones in the same subgroup.
Small repetitive payloads (JSON snapshots and deltas, telemetry, captions) compress several times better than they do alone, and the unit of loss is unchanged because the window never spans data the transport may drop.
A track property declares the compression.
Payloads remain opaque to relays, which forward a compressed track unchanged and need not implement this document.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}

A **compression scope** is the sequence of object payloads that share one DEFLATE window ({{scope}}).


# Introduction
A live track is usually many small payloads rather than a few large ones.
DEFLATE {{RFC1951}} applied to one such payload alone is close to useless: the window starts cold, and below a few hundred bytes the block overhead can exceed the savings.
The redundancy worth exploiting is *between* payloads, not within them: a JSON snapshot followed by its deltas, a telemetry record repeated at 50 Hz, successive subtitle cues.

Compressing a whole track or session captures that redundancy but is incompatible with how MoQ delivers.
Groups are dropped under congestion, arrive out of order, and a subscriber joins at an arbitrary point.
A window spanning data the subscriber never received cannot be reconstructed, so a single drop would end the track.

This document takes the middle position: the window is scoped to what the transport already delivers ordered and complete, and resets at every boundary.
A dropped or late group costs nothing beyond itself, and joining mid-track costs only the current group.
Compression stays invisible to the transport, so a relay routes, caches, and drops a compressed track exactly as it does any other.

{{hang}} already compresses its metadata tracks this way.
This document specifies the format independently so that any track can use it, and registers a property that declares it on the wire.


# Compression Scope {#scope}
The compression scope is the **subgroup**: the object payloads of one subgroup, in ascending object order, share one DEFLATE window.
A subgroup is the largest unit {{moqt}} delivers reliably and in order, which is exactly the requirement.

Each scope is independent and begins with a cold window.
An endpoint MUST NOT carry DEFLATE state across scopes, and MUST key its state by the enclosing track, group, and subgroup rather than by the stream that happens to carry them: a FETCH delivers objects from many subgroups over one stream.

An object delivered as a datagram is its own scope, because datagrams are neither ordered nor reliable: a scope MUST NOT span datagrams.
A cold window rarely pays for one small payload, so a track delivered as datagrams gains little from this document.

An object with a zero-length payload, including one carrying only an object status, is not part of the scope: it neither advances nor resets the window.

On {{moql}}, which has no subgroups, the scope is the group: a group is one ordered stream of frames, and a datagram carries an entire single-frame group.


# Compressed Stream {#stream}
The payloads of a scope, concatenated in object order, are compressed into a single raw DEFLATE stream {{RFC1951}}, with no zlib or gzip wrapper.

A publisher MUST perform a sync flush after each payload: an empty stored block ({{RFC1951, Section 3.2.4}}) that ends the DEFLATE block, byte-aligns the output, and retains the window.
Each object carries exactly the bytes the DEFLATE stream produced for its payload, with no length prefix; the transport already frames objects.
The result is that each object is independently addressable on the wire while still compressing against its predecessors.

A sync flush always ends with the fixed empty-block marker `0x00 0x00 0xff 0xff`.
A publisher MUST omit this trailing marker from each object and a consumer MUST append it before decompressing, the same trick as permessage-deflate ({{RFC7692, Section 7.2.1}}).

A zero-length payload is carried as a zero-length payload; a publisher MUST NOT feed it to the DEFLATE stream and a consumer MUST NOT feed it to the inflater.

A consumer MUST decompress a scope's objects in order, starting with the first.
A consumer that is missing an object MUST NOT decompress any later object in that scope; every later object in it is unrecoverable, and the consumer SHOULD abandon the scope (in {{moqt}} terms, stop reading the stream) rather than deliver corrupt payloads.

The compression level, block types, and flush strategy beyond the mandatory per-object sync flush are a publisher's choice.
Any {{RFC1951}}-conformant stream decodes.
A sync flush is `Z_SYNC_FLUSH` in zlib and its ports, so this format needs no DEFLATE implementation of its own.


# Declaring Compression {#declare}
The FLATE property declares that every object payload on a track is compressed per this document.
It is a track-level Key-Value-Pair ({{moqt}} Section 2.5), carried with the track's properties and delivered in SUBSCRIBE_OK or TRACK_STATUS ({{moqt}} Section 12).
Because the value is a single integer, FLATE uses an even Type so the value is a bare varint with no length prefix:

~~~
FLATE Track Property {
  Type (vi64) = 0xF1A7E
  Value (vi64) = 1
}
~~~

**Value**:
`1` means every object payload on the track is compressed per this document.
`0` is equivalent to absence.
A consumer MUST treat any other value as malformed and reject the track.

Absence means the track is uncompressed.
A consumer MUST NOT infer compression from the payload bytes: raw DEFLATE has no magic number, and a wrong guess yields plausible garbage.

The property is fixed for the lifetime of the track and MUST NOT change; compression is a property of the track's encoding, not of one subscription.
A relay MUST forward the property unchanged and MUST NOT compress or decompress on an endpoint's behalf.

This is a declaration, not a negotiation.
{{moqt}} has a receiver ignore a property it does not understand, so the property alone does not stop a consumer that has never heard of this document from handing compressed bytes to its application.
A publisher therefore compresses a track only where the application expects compressed tracks, and sends the property so that a consumer that does implement this document never has to derive the encoding from a name.

An application whose transport has no track properties, such as {{moql}}, declares compression out of band instead: a catalog field, or a name convention such as the `.z` track-name suffix of {{hang}}.
Either way the declaration is explicit.


# Security Considerations
A shared window is a side channel.
The compressed size of one payload reveals how much it has in common with the payloads before it in the same scope, which is enough to recover a secret an attacker can partially guess, as CRIME and BREACH demonstrated against TLS and HTTP compression.
A publisher MUST NOT place a secret and attacker-influenced data in the same compression scope.
A new group per trust domain is the straightforward remedy, since a scope never spans groups.
Transport encryption does not mitigate this: the sizes are visible to anyone who can count bytes on the wire, including every relay on the path.

Decompression is an amplifier.
A few bytes can inflate to gigabytes, so a consumer MUST bound the decompressed size of each object and abandon the scope once the bound is exceeded, rather than trusting any size the publisher declares.
A consumer SHOULD also bound the number of scopes it decompresses concurrently, because each holds a window (up to 32 KiB) for as long as its subgroup is open.

Compression changes an endpoint's traffic profile, which may reveal properties of the content to an observer that plaintext sizes would not.
It gives a relay no new access: payloads stay opaque, and a relay that implements nothing in this document forwards a compressed track correctly.


# IANA Considerations

This document requests one registration in the "MOQ Properties" registry ({{moqt}} Section 15.8), whose policy is Specification Required.
A high, distinctive value is requested to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions; it also avoids the greasing pattern (`0x7f * N + 0x9D`).

| Value   | Name  | Scope | Reference     |
|:--------|:------|:------|:--------------|
| 0xF1A7E | FLATE | Track | This Document |

The Type is even, so the value is a bare varint with no length prefix (see {{moqt}} Section 2.5).
This document defines only the values 0 and 1.
An extension that specifies a different compression algorithm registers its own property rather than overloading this one, so that a consumer never has to understand an algorithm to know it cannot decode the track.


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
