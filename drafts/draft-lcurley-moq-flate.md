---
title: "DEFLATE Compressed Tracks for MoQ"
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
This is a payload encoding, not a transport extension: it adds nothing to the wire, requests no code point, and is declared by the application that publishes the track.
A relay forwards a compressed track exactly as it does any other and never needs a DEFLATE implementation.

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
This document specifies the format independently so that any track can use it, and so that an implementation has something to conform to that is not tied to one catalog format.


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
Compression is declared by the application, never by the transport.
A consumer MUST know before it reads a track whether the track is compressed, and MUST NOT infer it from the payload bytes: raw DEFLATE has no magic number, and a wrong guess yields plausible garbage.

The conventional declaration is a `.z` suffix on the track name, which is what {{hang}} uses for its compressed metadata tracks.
An application with a catalog MAY declare it there instead.
Either way the declaration is explicit and belongs to the layer that already tells a consumer how to interpret the track's payloads.

This document deliberately defines no transport property, for two reasons.

A property that the transport carries has to be either ignorable or mandatory, and both are worse than the application declaring it.
Ignorable means a consumer that predates this document hands compressed bytes to its application; mandatory means every relay on the path has to understand the property before a compressed track can traverse it at all.

More fundamentally, a transport-level declaration makes the compression window the transport's business.
A relay drops objects, serves a FETCH over a subgroup it holds only part of, and re-frames what it forwards; if the transport owned the window, the relay would be responsible for emitting a stream that still decodes, which means carrying a DEFLATE implementation.
Keeping the encoding inside the payload keeps that responsibility with the endpoints: a relay forwards opaque bytes, and a consumer that receives an incomplete scope simply cannot decode the rest of it ({{stream}}).


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
It gives a relay no new access: payloads stay opaque either way, and a relay that has never heard of this document forwards a compressed track correctly.


# IANA Considerations

This document requests no registrations.
The format lives entirely inside object payloads, which {{moqt}} treats as opaque, so it adds no message, parameter, property, or version to the wire.


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
