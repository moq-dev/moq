---
title: "Media over QUIC - Group Compression"
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
  moql: I-D.lcurley-moq-lite
  moqt: I-D.ietf-moq-transport
  DEFLATE: RFC1951

informative:
  PMDEFLATE: RFC7692
  json:
    title: "Media over QUIC - JSON Tracks"
    target: https://datatracker.ietf.org/doc/draft-lcurley-moq-json/
    author:
      - fullname: Luke Curley
    date: false

--- abstract

This document specifies a compression layer for Media over QUIC tracks: the frames of a group are compressed as a single DEFLATE stream, sync-flushed at each frame boundary.
Every frame remains individually framed by the transport while later frames reuse the earlier ones as context, so a stream of similar payloads (a snapshot followed by deltas, repeated records, log lines) compresses far better than each frame alone.
The group is the compression unit, so groups remain independently decodable and relays remain oblivious.
The encoding is defined for both moq-lite and moq-transport.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}


# Introduction
Non-media tracks often carry small, repetitive frames: a catalog snapshot followed by deltas, a log of similar JSON records {{json}}, captions, telemetry.
Compressing each frame alone wastes the redundancy between frames, which is where most of the savings are.

This document compresses all the frames of a group through one shared DEFLATE {{DEFLATE}} context instead.
The group is the natural unit: its frames are already delivered reliably and in order, and it is the boundary at which subscribers join and caches evict.
Each group starts a fresh context, so any group is decodable on its own and dropping old groups loses nothing.

The layer is invisible to the transport.
Frame boundaries, timestamps, and group structure are unchanged; only the payload bytes differ.
Relays forward compressed tracks like any other.


# Terminology
This document is written using moq-lite {{moql}} terminology and applies equally to moq-transport {{moqt}}:

| moq-lite | moq-transport |
|:---------|:--------------|
| Track | Track |
| Group | Group |
| Frame | Object |

Decoding depends on receiving every frame of a group, in order.
Over moq-transport, a publisher MUST send all Objects of a group on a single Subgroup, with Object IDs assigned sequentially from 0 and no gaps.
Datagrams MUST NOT be used.

Whether a track is compressed is communicated out-of-band, typically by the application's catalog or a [naming convention](#track-naming).


# Encoding
A publisher maintains one raw DEFLATE compression context per group, created when the group starts.

Each frame's payload is produced by:

1. Compressing the uncompressed payload into the group's context.
2. Flushing to a byte boundary with an empty stored block (zlib's `Z_SYNC_FLUSH`), which always ends in the 4 bytes `0x00 0x00 0xff 0xff`.
3. Removing those trailing 4 bytes.

The result is the frame's payload on the wire.
The flush makes each frame self-delimited while retaining the sliding window, and the removed tail is constant so it carries no information; this is the technique of {{PMDEFLATE, Section 7.2.1}}, with the transport's frame boundaries taking the place of message framing.

The stream is raw DEFLATE: no zlib or gzip wrapper, matching `deflate-raw` in web runtimes.
The context MUST be reset at each new group and MUST NOT be shared across groups or tracks.

The DEFLATE window is at most 32 KiB of decompressed history, so a frame only benefits from context within that distance; a publisher whose reference frame (e.g. a snapshot) is much larger should not expect cross-frame savings beyond it.


# Decoding
A subscriber maintains one DEFLATE decompression context per group and MUST process the group's frames in order, starting at the first frame.
For each frame, the subscriber appends `0x00 0x00 0xff 0xff` to the payload and inflates the result through the group's context; the output is the uncompressed payload.

A frame cannot be decoded without every prior frame of its group.
A subscriber that joins a track mid-group, or loses a frame, MUST NOT attempt to decode subsequent frames of that group and SHOULD wait for the next group.

A subscriber MUST enforce a limit on each frame's decompressed size and treat a frame exceeding it as malformed: a small compressed frame can inflate to a far larger output, and the transport's frame size says nothing about the decompressed size.


# Track Naming
This section is a convention, not a requirement; the application's catalog is authoritative.

A compressed track appends `.z` to the name the track would otherwise have: `catalog.json.z`, `video.timeline.z`.
A publisher MAY publish uncompressed and compressed siblings carrying the same content, letting each subscriber pick.


# Security Considerations
The considerations of the underlying transport ({{moql}} or {{moqt}}) apply.

Compressed tracks are a decompression-bomb vector: a receiver MUST NOT size buffers from the compressed length and MUST enforce the decompressed-size limit of [Decoding](#decoding).

Compression leaks information through size.
If attacker-influenced data shares a compression context with confidential data, the compressed sizes can reveal the confidential data (as in the CRIME attack on TLS).
An application SHOULD NOT mix secrets and attacker-controlled content in the same group, or SHOULD leave such tracks uncompressed.


# IANA Considerations
This document has no IANA actions.


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
