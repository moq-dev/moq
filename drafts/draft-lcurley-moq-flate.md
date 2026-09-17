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
Nothing is added to the wire: the application declares the track compressed, and a relay forwards it unchanged.

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

A publisher MUST NOT send a compressed track in datagrams, which are neither ordered nor reliable.

An object with no payload is skipped, neither advancing nor resetting the window.


# Compressed Stream {#stream}
A subgroup's payloads are compressed, in order, into one raw DEFLATE stream {{RFC1951}}, with no zlib or gzip wrapper.
A publisher MUST sync flush after each payload, which ends the block and byte-aligns the output while retaining the window ({{RFC1951, Section 3.2.4}}, `Z_SYNC_FLUSH` in zlib).
Each object carries the bytes that flush produced, with no length prefix.

A sync flush always ends with the marker `0x00 0x00 0xff 0xff`.
A publisher MUST omit it from each object and a consumer MUST append it before decompressing, the same trick as permessage-deflate ({{RFC7692, Section 7.2.1}}).

A zero-length payload is neither compressed nor decompressed.

The stream is never terminated: no object ends in a final block, so a consumer decompresses incrementally and MUST NOT treat the absent end of stream as truncation.

A consumer missing an object MUST abandon the rest of the subgroup, which it can no longer decompress.
The compression level is a publisher's choice; any conformant stream decodes.


# Declaring Compression {#declare}
The application declares a track compressed, conventionally with a `.z` suffix on the track name, or in its catalog.
A consumer MUST NOT infer compression from the payload: raw DEFLATE has no magic number, so a wrong guess yields plausible garbage.

There is deliberately no transport signal.
A relay drops objects and re-frames what it forwards, so a transport that knew about the window would make the relay responsible for emitting a stream that still decodes, which means carrying a DEFLATE implementation.
Instead a relay forwards opaque bytes, and an endpoint that has never heard of this document never asks for a compressed track.


# Security Considerations
A shared window is a side channel.
The compressed size of one payload reveals how much it has in common with the payloads before it, enough to recover a secret an attacker can partially guess, as CRIME and BREACH demonstrated against TLS and HTTP compression.
A publisher MUST NOT compress a secret and attacker-influenced data in the same subgroup.
Encryption does not mitigate this, since the sizes are visible to anyone on the path.

A few bytes can inflate to gigabytes, so a consumer MUST bound the decompressed size of each object and abandon the subgroup once that bound is exceeded.
Each open subgroup also holds a window of up to 32 KiB, so a consumer SHOULD bound how many it decompresses at once.


# IANA Considerations

This document requests no registrations.
The format lives inside object payloads, which {{moqt}} treats as opaque.


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
