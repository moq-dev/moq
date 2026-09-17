---
title: "MoQ MPEG-TS Catalog Extension"
abbrev: "moq-mpegts"
category: info

docname: draft-lcurley-moq-mpegts-latest
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
  hang: I-D.lcurley-moq-hang
  msf: I-D.ietf-moq-msf
  mpeg2:
    title: "Information technology - Generic coding of moving pictures and associated audio information: Systems"
    target: https://www.iso.org/standard/83239.html
    author:
      - org: ISO/IEC
    seriesinfo:
      ISO/IEC: 13818-1
    date: 2023
informative:
  msfts: I-D.gregoire-moq-msfts
  dvbrep:
    title: "Digital Video Broadcasting (DVB); Guidelines on implementation and usage of Service Information (SI)"
    target: https://www.etsi.org/standards
    author:
      - org: ETSI
    seriesinfo:
      ETSI: TS 101 211
    date: false
  scte35:
    title: "Digital Program Insertion Cueing Message"
    target: https://www.scte.org/standards/
    author:
      - org: SCTE
    seriesinfo:
      SCTE: "35"
    date: false

--- abstract

This document defines the `mpegts` catalog section, which records the MPEG-TS identity of a broadcast that was demultiplexed out of an MPEG-2 Transport Stream {{mpeg2}}: the PID and PMT descriptors of each track, the program identity, the service information tables, and a carriage record for every elementary stream the publisher did not decode.
Media itself stays in the catalog's ordinary codec-neutral track descriptions, so a subscriber that ignores this section still plays the broadcast.
A subscriber that reads it can rebuild a transport stream whose PIDs, descriptors, signaling, and undecoded streams match the source.
The section is defined once and carried as a root member of either the hang catalog {{hang}} or the MSF catalog {{msf}}.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}

This document uses the terminology of {{mpeg2}} for PID, PSI, PAT, PMT, PES, descriptor, section, and stream_type, and the terminology of {{hang}} for broadcast, track, group, and frame.

A **demultiplexed** broadcast is one whose publisher split a transport stream into one MoQ track per elementary stream.
A **verbatim** track carries an elementary stream the publisher did not decode, byte-for-byte.


# Introduction {#introduction}
A transport stream reaches MoQ in one of two shapes.

A publisher can leave the multiplex intact and carry the packet stream as opaque payload, which is what {{msfts}} specifies.
Nothing is parsed, nothing is lost, and the relay sees one track of bytes.

Or the publisher can demultiplex, which is what this document addresses.
Each elementary stream becomes its own MoQ track with a real codec description, so a relay can drop, prioritize, and cache per track, a subscriber can take the audio without the video, and a browser can hand frames to a decoder without a transport-stream parser.

Demultiplexing loses everything the multiplex carried that is not media: the PID layout, the PMT descriptors, the program identity, the service information, and any elementary stream the publisher has no decoder for.
That loss is invisible until someone asks for a transport stream back, at which point the rebuilt multiplex is a different stream: renumbered PIDs, no service name, no teletext, no SCTE-35 {{scte35}}.
Contribution and broadcast workflows treat those as part of the signal, not as decoration.

The `mpegts` section carries exactly that residue, and nothing a codec-neutral catalog already describes.
It is additive: a subscriber that does not implement this document sees an ordinary broadcast and ignores the section, per the catalog's own rule for unrecognized root members.

This document does not define a packaging or a container.
Decoded media keeps the container its catalog entry declares; verbatim tracks use the framing in {{verbatim}}.


# The mpegts Section {#section}
A publisher that demultiplexed a transport stream SHOULD add an `mpegts` root member to its catalog:

~~~
type Mpegts = {
  "tracks": Map<TrackName, Track> | undefined,
  "programDescriptors": Descriptor[] | undefined,
  "program": Program | undefined,
  "si": Map<PidString, Si> | undefined,
}
~~~

Every member is optional and a publisher MUST omit an empty one, so a broadcast with nothing to record omits the section entirely.

The section describes the broadcast, not one rendition, so it lives at the catalog root rather than inside a track description.
Its carriage in each catalog format is defined in {{carriage}}.

The section is a *record of the source*, not a request.
A subscriber MAY rebuild a transport stream from it, MAY use it to route the verbatim tracks, and MAY ignore it entirely.

## tracks
`tracks` maps a MoQ track name to that track's MPEG-TS identity:

~~~
type Track = {
  "pid": number,
  "descriptors": Descriptor[] | undefined,
  "verbatim": Verbatim | undefined,
}
~~~

`pid` is the track's PID in the source, an integer in 0..8191.
A publisher MUST NOT record 0x0000 (PAT) or 0x1FFF (null packets), which carry no elementary stream.

`descriptors` are the track's ES-level descriptors from the PMT ({{descriptor}}), in PMT order.

`verbatim` is present when the track carries an undecoded elementary stream ({{verbatim}}) and absent when the track is decoded media described elsewhere in the catalog.

A decoded track MAY appear here with only its `pid` and `descriptors`; that is how an ISO-639 language descriptor or a registration descriptor survives.

An entry naming a track the catalog does not otherwise describe and that has no `verbatim` record describes nothing, and a consumer MUST ignore it.

## programDescriptors
The PMT's program-level descriptors (`program_info`), in PMT order ({{descriptor}}).

## program
The program identity from the PAT:

~~~
type Program = {
  "transportStreamId": number,
  "programNumber": number,
  "pmtPid": number,
}
~~~

These three values are the only part of the service layer this document parses, because a rebuilt PAT and PMT must agree with the opaque tables carried in `si`.
Everything else about the program, including its name, provider, type, and network, stays inside those tables.

`transportStreamId` is the PAT's `transport_stream_id`.
`programNumber` is the program's number in the PAT, which DVB calls the service id.
`pmtPid` is the PID the PMT rode on.

The member is named for the MPEG concept rather than the DVB one: the same PAT fields describe an ATSC or ISDB stream.

`program` is absent for a broadcast that never came from a transport stream.
A publisher MUST include it when one is known, since without it a rebuilt stream carries a synthesized identity that contradicts the carried `si`.

## si
The standalone service information tables, keyed by the PID they ride on:

~~~
type Si = {
  "sections": string[],
  "interval": number | undefined,
}
~~~

JSON object keys are strings, so a PID key is its decimal value with no leading zeros, `"17"` for 0x0011.
A consumer MUST ignore an entry whose key is not such an integer in 0..8191.

`sections` are complete sections ({{mpeg2}} Section 2.4.4), each including its header and CRC, base64-encoded ({{!RFC4648, Section 4}}).
A section is opaque: nothing in this document parses one, so an SDT, a NIT, a BAT, or a table the publisher has never heard of all survive the same way.
A PID carries a *set* of sections (a multi-service SDT is several, and the SDT PID also carries the BAT), so they are held together and listed in the order first seen.
A publisher MUST replace a section in place when it is re-signaled with the same `table_id`, `table_id_extension`, and `section_number`, and MUST NOT append a duplicate: SI repeats every few seconds, and appending would grow the catalog without bound.

`interval` is how often a rebuilt stream repeats this PID's sections, in milliseconds.
It is a hint and a bound, not the source's observed cadence, which is a property of that multiplexer's bitrate shaping and means nothing downstream.
A publisher SHOULD use the maximum repetition interval its delivery system defines, for example {{dvbrep}} for DVB: 10000 for the NIT PID and 2000 for the SDT/BAT PID.
A publisher MUST omit `interval` for a PID whose repetition requirement it does not know; a consumer then repeats those sections on its own PSI cadence, so an unrecognized table degrades to a safe rate rather than being dropped.

PSI proper (PAT and PMT) is never carried here: it is rebuilt from `program`, `programDescriptors`, and the per-track entries.

## Descriptor {#descriptor}
One descriptor, carried verbatim:

~~~
type Descriptor = {
  "tag": number,
  "data": string,
}
~~~

`tag` is the `descriptor_tag` (0x05 registration, 0x0A ISO-639 language, ...), an integer in 0..255.
`data` is the descriptor body after the tag and length, base64-encoded ({{!RFC4648, Section 4}}).

Descriptors are opaque, so a descriptor this document has never heard of round-trips like any other.
A consumer MUST re-emit a descriptor list in the order given, and MUST NOT reorder, merge, or drop entries it does not recognize.


# Verbatim Tracks {#verbatim}
An elementary stream the publisher does not decode is carried on its own MoQ track, byte-for-byte, described by a `verbatim` record:

~~~
type Verbatim = {
  "streamType": number,
  "framing": "pes" | "section",
  "streamId": number | undefined,
}
~~~

`streamType` is the PMT `stream_type` to re-announce: 0x86 for SCTE-35 {{scte35}}, 0x06 for private PES, 0x05 for private sections, and so on.

`framing` says how the payload is framed, so a consumer knows how to repacketize it.
If absent it defaults to `"pes"`.
A consumer MUST ignore a track whose `framing` it does not recognize, rather than guess.

`streamId` is the original PES `stream_id`, for example 0xBD (`private_stream_1`) for teletext, DVB subtitles, and DVB AC-3, or 0xC0-0xDF for audio.
It is meaningful only with `"pes"` framing.
A publisher SHOULD record it, because strict broadcast demultiplexers and stream analyzers reject a PES relabeled under a different id; a consumer that has none falls back to `private_stream_1`.

## Framing
With `"pes"` framing, each frame is one complete PES payload, one access unit, timestamped with its PTS.
A PES that carried no PTS is timestamped 0.

With `"section"` framing, each frame is one complete section, including its header and CRC.
A section carries its own timing inside the payload (a splice time, for example), so the frame timestamp is the media time at which the section arrived, not a presentation time.

Each frame MUST be a keyframe in its own group, whatever the framing.
A verbatim payload is all-or-nothing: half a section or half an access unit is not a smaller one, so a group that could be partially delivered would hand the consumer bytes it cannot use.

A verbatim track has no entry in the host catalog's own track list ({{description}}), so nothing there declares its container.
Its frames therefore use hang's `legacy` container ({{hang}}) in either catalog format: a varint timestamp in microseconds followed by the payload above.

## Description
A verbatim track carries no codec, no dimensions, and no decoder configuration, because the publisher has none: `streamType` and the PMT descriptors are the whole description.
A publisher therefore MUST NOT describe a verbatim track as a media rendition in the host catalog, and MUST describe it only by its entry in `tracks`.

A consumer that does not implement this document never learns such a track exists, which is the intended outcome: it holds bytes that only a transport-stream consumer can use.


# Carriage {#carriage}
The section is the same JSON in either catalog format, so one encoder and one parser serve both.

## hang {#carriage-hang}
The section is a root member named `mpegts` of the hang catalog, alongside `video` and `audio` ({{hang}}).
Note that hang carries a decoder config's raw bytes as hex, while every binary field in this section is base64; the two alphabets overlap, so the encoding cannot be detected and is stated per field above.

## MSF {#carriage-msf}
The section is a root member named `mpegts` of the MSF catalog {{msf}}, alongside `tracks`.
A publisher MUST NOT name a section after a member MSF itself defines.

The keys of the section's own `tracks` member are MSF track names.
A decoded track is described by its MSF track object as usual; a verbatim track has no MSF track object ({{verbatim}}).

This document neither defines nor uses an MSF packaging value.
{{msfts}} registers `m2ts` for the passthrough shape described in {{introduction}}, where the multiplex is never split; the two are alternatives, and a track is one or the other.


# Rebuilding a Transport Stream {#rebuild}
A consumer rebuilding a transport stream from a broadcast that carries this section:

- MUST place each track on the `pid` recorded for it, and MUST assign an unused PID to any track with no recorded one.
- MUST put the PMT on `program.pmtPid` and build the PAT and PMT from `program`, so the identity agrees with the carried `si`.
- MUST re-emit each track's `descriptors` as its ES-level descriptors and `programDescriptors` as the PMT's `program_info`.
- MUST re-emit each `si` PID's sections byte-for-byte on that PID, at least as often as its `interval` when one is declared.
- MUST repacketize each verbatim track per its `framing` and `streamType`, using `streamId` when one is recorded.

With no `program` the consumer synthesizes an identity, and SHOULD then omit any carried `si`, which describes a program that no longer exists.

A consumer MAY derive signaling the section does not carry when it is implied: a program carrying a section-framed 0x86 stream and no `programDescriptors` implies the SCTE-35 `CUEI` registration descriptor.
A consumer MUST NOT derive one that contradicts a recorded descriptor.


# Security Considerations
Every binary field in this section (a descriptor body, an SI section, a verbatim payload) is opaque to the publisher that recorded it and to the relay that carries it, and is re-emitted without inspection.
A consumer MUST treat all of it as untrusted input.
A publisher of this section is asking a consumer to re-emit bytes it never parsed, so a consumer that parses them later inherits whatever the original multiplex contained.

A malicious or broken catalog can make a consumer allocate: `si` has no bound on the number of PIDs or sections per PID, `tracks` none on the number of entries, and each is base64 in a catalog that is republished on every change.
A consumer MUST bound the size of the section it accepts and the number of entries it keeps, and MUST reject a section it cannot bound rather than truncate it into a stream that silently differs from what was described.

PIDs, program numbers, and stream types are 13-, 16-, and 8-bit values in a JSON document that can hold any number.
A consumer MUST range-check every one before using it, and MUST reject the section rather than truncate a value into a valid-looking PID.
A catalog can name the same PID for two tracks, or a PID reserved for PSI; a consumer MUST detect the collision and MUST NOT emit a stream in which two elementary streams share a PID.

This section makes a broadcast's provenance legible: the service name, provider, and network inside the carried SI, and the original PID layout, all become readable by anything that can read the catalog, including relays. A publisher that does not want that MUST omit the section, which costs only the ability to rebuild the source multiplex.


# IANA Considerations

This document requests no registrations.

Should {{msf}} establish a registry of catalog root members, this document requests registration of `mpegts` with this document as the reference.


--- back

# Appendix A: Example
{:numbered="false"}

A broadcast demultiplexed from a DVB transport stream: H.264 video and an AAC track described as ordinary renditions, a verbatim SCTE-35 stream, and the source's SDT carried opaquely.

~~~
{
  "mpegts": {
    "program": {
      "transportStreamId": 4660,
      "programNumber": 1,
      "pmtPid": 100
    },
    "programDescriptors": [
      { "tag": 5, "data": "Q1VFSQ==" }
    ],
    "tracks": {
      "video0": { "pid": 257 },
      "audio0": {
        "pid": 258,
        "descriptors": [ { "tag": 10, "data": "ZW5nAA==" } ]
      },
      "0.ts": {
        "pid": 500,
        "verbatim": { "streamType": 134, "framing": "section" }
      }
    },
    "si": {
      "17": { "sections": [ "QvAl..." ], "interval": 2000 }
    }
  }
}
~~~


# Appendix B: Changelog
{:numbered="false"}

## draft-lcurley-moq-mpegts-00
{:numbered="false"}

- Initial version.


# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
