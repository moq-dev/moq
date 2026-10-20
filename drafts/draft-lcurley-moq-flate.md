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

--- abstract

This document specifies how a MoQ Transport {{moqt}} track carries DEFLATE-compressed payloads.
Each subgroup is one raw DEFLATE stream, sync flushed at each object boundary, so every object stays self-delimited while later objects compress against the earlier ones in the subgroup.
Small repetitive payloads compress several times better than they do alone, and a dropped group costs nothing beyond itself because the window never spans one.
The transport declares a track compressed and carries each object's inflated size, so a relay forwards the compressed bytes to a peer that understands them and inflates them for one that does not.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}


# Introduction
A live track is usually many small payloads rather than a few large ones.
DEFLATE {{RFC1951}} on one such payload alone is close to useless: the window starts cold, and below a few hundred bytes the block overhead can exceed the savings.
The redundancy worth exploiting is between payloads: a JSON snapshot followed by its deltas, a telemetry record repeated at 50 Hz, successive subtitle cues.

Compressing a whole track captures that redundancy but breaks on delivery.
Groups are dropped, arrive out of order, and a subscriber joins at an arbitrary point, so a window spanning data it never received cannot be reconstructed.
Scoping the window to a subgroup is the compromise: a drop costs nothing beyond itself, joining costs only the current group, and the transport never has to know.


# Compression Scope {#scope}
Each subgroup is a separate DEFLATE stream: its object payloads share one window, in order, starting cold.
{{moql}} has no subgroups, so each group is one stream of frames.

The application decides which subgroups a track uses; a consumer knows which to expect and decodes each one's objects in order.

A datagram carries a group of one object, so it is a stream of its own: compressed alone, starting cold.

An object with no payload is skipped, neither advancing nor resetting the window.


# Compressed Stream {#stream}
A subgroup's payloads are compressed, in order, into one raw DEFLATE stream {{RFC1951}}, with no zlib or gzip wrapper.
A publisher MUST sync flush after each payload, which ends the block and byte-aligns the output while retaining the window ({{RFC1951, Section 3.2.4}}, `Z_SYNC_FLUSH` in zlib).
Each object carries the bytes that flush produced, with no length prefix.

A sync flush always ends with the marker `0x00 0x00 0xff 0xff`.
A publisher MUST omit it from each object and a consumer MUST append it before decompressing, the same trick as permessage-deflate ({{RFC7692, Section 7.2.1}}).

A zero-length payload is neither compressed nor decompressed.

The stream is never terminated: a publisher MUST NOT emit a final block, and a consumer MUST treat one as malformed and abandon the subgroup.
A consumer decompresses incrementally and MUST NOT treat the absent end of stream as truncation.

A consumer missing an object MUST abandon the rest of the subgroup, which it can no longer decompress.
The compression level is a publisher's choice; any conformant stream decodes.


# Declaring Compression {#declare}
The transport declares a track compressed, and gives each object's inflated size beside its compressed one.
{{moql}} does both from moq-lite-07: a Compression codec id in TRACK_INFO, and an Inflated Size on each FRAME and datagram.
A consumer MUST NOT infer compression from the payload: raw DEFLATE has no magic number, so a wrong guess yields plausible garbage.

A consumer MUST treat an object that inflates to more or less than its declared size as malformed, and abandon the subgroup.

A relay forwards the compressed objects as received to a peer whose transport declares compression.
For any other peer it inflates them, once per subgroup however many such peers it serves, and sends the original objects without the declaration.
A peer is never sent compressed bytes it was not told about.


# Security Considerations
A shared window is a side channel.
The compressed size of one payload reveals how much it has in common with the payloads before it, enough to recover a secret an attacker can partially guess, as CRIME and BREACH demonstrated against TLS and HTTP compression.
A publisher MUST NOT compress a secret and attacker-influenced data in the same subgroup.
Encryption does not mitigate this, since the sizes are visible to anyone on the path.

A few bytes can inflate to gigabytes, so a consumer MUST check each declared inflated size against its limits before decompressing, and never inflate past it.
Each open subgroup also holds a window of up to 32 KiB, so a consumer SHOULD bound how many it decompresses at once.


# IANA Considerations

This document requests no registrations.
The format lives inside object payloads, and the declaration in the transport that carries them.


--- back

# Changelog

## Since draft-lcurley-moq-flate-00 (in progress)

- Compression is declared by the transport instead of the application: {{moql}} carries a Compression codec in TRACK_INFO and each object's Inflated Size on FRAME and datagrams.
- A relay inflates for a peer whose transport does not declare compression.
- A datagram is compressed alone, as a stream of its own.
- A consumer rejects an object that inflates to other than its declared size, or that ends in a final block.

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
