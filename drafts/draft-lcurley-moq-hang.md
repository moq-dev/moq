---
title: "Media over QUIC - Hang"
abbrev: "hang"
category: info

docname: draft-lcurley-moq-hang-latest
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
  webcodecs:
    title: "WebCodecs"
    target: https://www.w3.org/TR/webcodecs/
    author:
      - org: W3C
    date: false

informative:

--- abstract

Hang is a real-time conferencing protocol built on top of moq-lite.
A room consists of multiple participants who publish media tracks.
All updates are live, such as a change in participants or media tracks.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}


# Terminology
Hang is built on top of moq-lite [moql] and uses much of the same terminology.
A quick recap:

- **Broadcast**: A collection of Tracks from a single publisher.
- **Track**: A series of Groups, each of which can be delivered and decoded *out-of-order*.
- **Group**: A series of Frames, each of which must be delivered and decoded *in-order*.
- **Frame**: A sized payload of bytes representing a single moment in time.

Hang introduces additional terminology:

- **Room**: A collection of participants, publishing under a common prefix.
- **Participant**: A moq-lite broadcaster that may produce any number of media tracks.
- **Catalog**: A JSON document that describes each available media track, supporting live updates.
- **Container**: A tiny header in front of each media payload containing the timestamp.


# Discovery
The first requirement for a real-time conferencing application is to discover other participants in the same room.
Hang does this using moq-lite's ANNOUNCE capabilities.

A room consists of a path.
Any participants within the room MUST publish a broadcast with the room path as a prefix which SHOULD end with the `.hang` suffix.

For example:

~~~
/room123/alice.hang
/room123/bob.hang
/room456/zoe.hang
~~~

A participant issues an ANNOUNCE_PLEASE message to discover any other participants in the same room.
The server (relay) will then respond with an ANNOUNCE message for any matching broadcasts, including their own.

For example:

~~~
ANNOUNCE_PLEASE prefix=/room/
ANNOUNCE suffix=alice.hang active=true
ANNOUNCE suffix=bob.hang   active=true
~~~

If a participant leaves or is disconnected, their broadcast is unannounced.
Publishers and subscribers SHOULD terminate any subscriptions once a participant is unannounced.

~~~
ANNOUNCE suffix=alice.hang active=false
~~~

# Catalog {#catalog}
The catalog describes the available media tracks for a single participant.
It's a JSON document that extends the W3C WebCodecs specification {{webcodecs}}.

The catalog is published as a `catalog.json` track within the broadcast so it can be updated live as the participant's media tracks change.
A participant MAY forgo publishing a catalog if it does not wish to publish any media tracks now and in the future.

The catalog track consists of multiple groups, one for each update.
Each group contains a single frame with UTF-8 JSON.

A publisher MUST NOT write multiple frames to a group until a future specification includes a delta-encoding mechanism (via JSON Patch most likely).

A publisher SHOULD also serve the catalog as a `catalog.json.z` track: the identical JSON under the same group and frame rules, differing only by compression ({{compression}}).
A consumer reads whichever of the two tracks it prefers.

## Root
The root of the catalog is a JSON document with the following schema:

~~~
type Catalog = {
  "audio": AudioSchema | undefined,
  "video": VideoSchema | undefined,
  "timeline": TimelineSchema | undefined,
  "text": TextSchema | undefined,
  "json": JsonTracks | undefined,
  "binary": BinaryTracks | undefined,
  // ... any custom fields ...
}
~~~

Additional fields MAY be added based on the application.
The catalog SHOULD be mostly static, delegating any dynamic content to other tracks.

For example, a chat entry should name a chat track, not carry individual chat messages.
This way catalog updates are rare and a client MAY choose to not subscribe.
The `json` and `binary` sections ({{data}}) are how a track like that is listed.

This specification defines audio, video, and text media tracks, plus an optional timeline track ({{timeline}}) indexing their segments and the application data tracks in {{data}}.

## Video
A video track contains the necessary information to decode a video stream.


~~~
type VideoSchema = {
  "renditions": Map<TrackName, VideoDecoderConfig>,
  "display": {
    "width": number,
    "height": number,
  } | undefined,
  "rotation": number | undefined,
  "flip": boolean | undefined,
}
~~~

The `renditions` field contains a map of track names to video decoder configurations.
See the [WebCodecs specification](https://www.w3.org/TR/webcodecs/#video-decoder-config) for specifics and registered codecs.
Any field carrying raw bytes, notably `description`, is a hex string ({{binary}}).

The `display` field is the size to render the video at, in pixels.
It is separate from a rendition's `displayAspectWidth`/`displayAspectHeight` because changing it does not require reinitializing the decoder.

In addition to the WebCodecs fields, each rendition MAY carry the common rendition fields ({{common}}) plus:

~~~
type VideoDecoderConfigExtensions = {
  "displayAspectWidth": number | undefined,
  "displayAspectHeight": number | undefined,
  "stalled": boolean | undefined,
}
~~~

`displayAspectWidth` and `displayAspectHeight` give the display aspect ratio of the media, stretching or shrinking the coded pixels.
A consumer that understands neither field MUST assume square pixels, a 1:1 ratio.
Both MUST be present together; a consumer that sees only one MUST ignore it.

`stalled` indicates that the publisher recommends temporarily avoiding the rendition.
The track remains available when `stalled` is true.
A consumer SHOULD select an unstalled rendition when it supports one, but MAY select a stalled rendition when no unstalled rendition is suitable.
If absent, `stalled` defaults to false.

For example:

~~~
{
  "renditions": {
    "720p": {
      "label": "HD",
      "codec": "avc1.64001f",
      "container": { "kind": "legacy" },
      "codedWidth": 1280,
      "codedHeight": 720,
      "bitrate": 6000000,
      "stalled": true,
      "framerate": 30.0,
      "jitter": 34
    },
    "480p": {
      "codec": "avc1.64001e",
      "container": { "kind": "legacy" },
      "codedWidth": 848,
      "codedHeight": 480,
      "bitrate": 2000000,
      "framerate": 30.0,
      "jitter": 34
    }
  },
  "display": {
    "width": 1280,
    "height": 720
  },
  "rotation": 0,
  "flip": false,
}
~~~


## Audio
An audio track contains the necessary information to decode an audio stream.

~~~
type AudioSchema = {
  "renditions": Map<TrackName, AudioDecoderConfig>,
}
~~~

The `renditions` field contains a map of track names to audio decoder configurations.
See the [WebCodecs specification](https://www.w3.org/TR/webcodecs/#audio-decoder-config) for specifics and registered codecs.
Any field carrying raw bytes, notably `description`, is a hex string ({{binary}}).

In addition to the WebCodecs fields, each rendition MAY carry the common rendition fields ({{common}}).

### PCM {#audio-pcm}

Hang defines the `"pcm"` audio codec for uncompressed samples.
The `sampleRate` and `numberOfChannels` fields MUST be present and greater than zero.
The `description` field MUST NOT be present.
If `bitrate` is present, it MUST equal `sampleRate * numberOfChannels * 32`.

Each codec payload consists of interleaved IEEE 754 binary32 samples in little-endian byte order.
Samples are ordered by sample frame, then by ascending channel index within each frame.
The payload length MUST be a non-zero multiple of `4 * numberOfChannels`.
The frame timestamp identifies the presentation time of its first sample.
The frame duration in seconds is the payload length divided by `4 * numberOfChannels * sampleRate`.

For example:

~~~
{
  "renditions": {
    "stereo": {
      "label": "English stereo",
      "codec": "opus",
      "container": { "kind": "legacy" },
      "sampleRate": 48000,
      "numberOfChannels": 2,
      "bitrate": 128000,
      "jitter": 20
    },
    "mono": {
      "codec": "opus",
      "container": { "kind": "legacy" },
      "sampleRate": 48000,
      "numberOfChannels": 1,
      "bitrate": 64000,
      "jitter": 20
    }
  },
}
~~~

## Text
A text track carries timed text: captions or subtitles.
Unlike audio and video there is no WebCodecs decoder for text, so a consumer parses each cue directly and renders it as an overlay.

~~~
type TextSchema = {
  "renditions": Map<TrackName, TextConfig>,
}

type TextConfig = {
	"format": "vtt" | "ttml" | "utf8" | string,
	"role": "subtitle" | "caption" | string | undefined,
	"lang": string | undefined,
	// plus the common rendition fields
}
~~~

The `renditions` field maps track names to text configurations, typically one per language.

The `format` field selects the cue serialization, and tells a consumer how to parse each frame's payload:

- `vtt`: WebVTT ([W3C WebVTT](https://www.w3.org/TR/webvtt1/)). Each payload is a self-contained `WEBVTT` segment whose cues carry absolute timing. A cue's embedded start time MUST match its enclosing frame timestamp.
- `ttml`: TTML / IMSC1 ([W3C IMSC](https://www.w3.org/TR/ttml-imsc1.1/)) fragment (XML) with absolute timing. A cue's embedded start time MUST match its enclosing frame timestamp.
- `utf8`: raw UTF-8 text with no embedded timing or styling. The cue is shown from its frame timestamp until the next cue; an empty payload clears it.

A consumer MUST ignore a rendition whose `format` it does not recognize.

The `role` field describes the accessibility intent, defaulting to `subtitle`:

- `subtitle`: a transcription of the spoken dialogue, same-language or translated.
- `caption`: a textual representation of all audio, including non-speech sounds, for viewers who cannot hear it.

The vocabulary is expected to grow, so a consumer MUST NOT reject a rendition whose `role` it does not recognize.
It SHOULD keep such a rendition selectable and treat the role as `subtitle`, and MUST preserve the value verbatim if it republishes the catalog.
Unlike `format`, an unrecognized `role` never prevents rendering: it describes intent, not the wire.

The `lang` field is the BCP-47 {{!RFC5646}} language tag of the track, for example `en` or `es-419`.
Two renditions sharing a `lang` are told apart by `label` ({{field-label}}), one of the common rendition fields ({{common}}).

Regardless of `format`, each frame's timestamp ({{container}}) is the authoritative cue start time on the media clock, so a relay and a consumer can order and schedule cues without parsing the payload.
A text track has no delta frames: every frame is a self-contained cue, so a group MAY consist of multiple frames, following the same rule as a codec that lacks delta frames ({{container}}).

For example:

~~~
{
  "renditions": {
    "captions.en": {
      "format": "vtt",
      "container": { "kind": "legacy" },
      "role": "caption",
      "lang": "en",
      "label": "English"
    },
    "subtitles.es": {
      "format": "vtt",
      "container": { "kind": "legacy" },
      "role": "subtitle",
      "lang": "es"
    }
  }
}
~~~

## Data Tracks {#data}
Beyond media, a broadcast MAY publish application data tracks: a chat log, a telemetry feed, a thumbnail, a serialized state blob.
The catalog lists them in two sections, split by payload encoding:

~~~
type JsonTracks = {
  "tracks": Map<TrackName, JsonSchema>,
}

type BinaryTracks = {
  "tracks": Map<TrackName, BinarySchema>,
}
~~~

A `json` track's frames are UTF-8 JSON values; a `binary` track's frames are opaque to everything but the application.
The split is by what a generic consumer can do without knowing the application: parse, inspect, and re-serialize a JSON track, but only copy a binary one.

Each map is keyed by track name, so an entry's key is the name to subscribe to.
Unlike the media sections these are not rendition sets: entries are distinct tracks, not alternatives to choose between.
A publisher MUST omit a section that holds no tracks.

A consumer discovers these tracks the same way it discovers media, by reading the catalog; nothing else in the broadcast announces them.

### JSON {#json-tracks}
~~~
type JsonSchema = {
  "mode": Mode,
  "compression": Compression | undefined,
  "schema": string | undefined,
  "broadcast": string | undefined,
  "timeline": TimelineSchema | undefined,
}
~~~

The `schema` field is an optional identifier for the shape of each value, typically the URL of a JSON Schema.
It is descriptive: a consumer that does not recognize it MUST still be able to read the track.

### Binary {#binary-tracks}
~~~
type BinarySchema = {
  "mode": Mode,
  "compression": Compression | undefined,
  "mime": string | undefined,
  "broadcast": string | undefined,
  "timeline": TimelineSchema | undefined,
}
~~~

The `mime` field is an optional media type ({{!RFC6838}}) for each payload, for example `image/jpeg`.
It is descriptive, the same as `schema` above.

### mode {#field-mode}
~~~
type Mode = "snapshot" | "stream"
~~~

The `mode` field says how a track's groups compose its frames into what a consumer sees.
It is REQUIRED and has no default: reading an append log as a latest-value document silently discards every payload but the last.
A consumer MUST ignore a track whose `mode` it does not recognize.

A `snapshot` track is lossy.
Each group is self-contained and supersedes the previous one, so a consumer reads only the newest group and a publisher MAY drop older ones.
A group's first frame is a complete value.
A `json` track MAY follow it with JSON Merge Patch ({{!RFC7396}}) deltas applied in order; a `binary` track MUST write exactly one frame per group, since an opaque payload has no delta form.

A `stream` track is lossless in the sense that nothing supersedes anything else.
It is a single group, never rolled, carrying one self-contained payload per frame, delivered in order.

A publisher that cannot write a payload MUST close the track rather than continue the log in a second group.
A second group would present a gap as if it were a complete log; ending the track surfaces the failure instead, and a publisher with more to say opens a new track.

A consumer MUST NOT skip to the newest group, since a later group does not supersede an earlier one the way a `snapshot` group does.
It reads the one group the track carries, and MUST fail the read if the track carries a second, since whatever would have completed the first group is gone and yielding the remainder would present that gap as a continuous log.
This is why a consumer takes groups in arrival order rather than by ascending sequence: groups are separate streams, so a second group MAY arrive with a lower sequence than the first, and skipping it would report a truncated log as a whole one.

A consumer MUST NOT wait for the first group to end before it looks for a second.
A publisher that opens a second group while the first is still open is broken in exactly the way this rule exists to catch, and a consumer that only checks at the group boundary waits forever on a group that publisher may never finish.
A consumer MAY yield payloads it has already received from the first group before it fails, since those payloads precede the gap, but it MUST fail rather than block once they run out.

Retention is bounded, and this is the limit of "lossless".
A group's cache is finite, so a publisher that writes more than it holds evicts the log's earliest frames.
A consumer that has not kept up, or that subscribes later, then cannot read the log at all: the read fails once it reaches the evicted prefix, rather than silently resuming at whatever the cache still holds.
That is the intended behaviour, since a partial log presented as a whole one is exactly what this mode exists to prevent, and under compression ({{compression}}) the retained frames are undecodable anyway without the evicted prefix as context.
A publisher SHOULD therefore keep a `stream` track's log within what its groups retain, and split anything unbounded across successive tracks; a consumer that needs the whole log SHOULD subscribe before the publisher exceeds that.

### compression {#field-compression}
~~~
type Compression = "deflate"
~~~

The `compression` field names the compression applied to the track's frames.
If absent, the frames are uncompressed.
A consumer MUST ignore a track whose `compression` it does not recognize, since it cannot decode the frames.

The `deflate` value is the group-scoped DEFLATE of {{compression}}.
A `snapshot` group covers a single value (plus any deltas), so its window spans that group alone; a `stream` group's frames compress against the earlier ones in the log.

### broadcast and timeline {#data-shared}
The `broadcast` field carries the same meaning here as it does for a media rendition ({{field-broadcast}}).
The `timeline` field advertises a companion timeline track indexing this track's groups, with the same schema as the catalog's root `timeline` field ({{timeline-catalog}}).

## Binary Fields {#binary}
A decoder config field carrying raw bytes, notably `description` (an `AllowSharedBufferSource` in WebCodecs), is carried in the catalog as a hex string ({{!RFC4648, Section 8}}).
A publisher SHOULD emit lowercase hexadecimal characters and MUST NOT emit a `0x` prefix or any separators.
A consumer MUST accept either case.

Note that this differs from the `cmaf` container's `init` field ({{container}}), which is base64 ({{!RFC4648, Section 4}}); the two alphabets overlap, so the encoding cannot be detected and must be specified.

## Common Rendition Fields {#common}
Audio, video, and text renditions share the following fields, extending the WebCodecs decoder config for audio and video:

~~~
type CommonExtensions = {
  "broadcast": string | undefined,
  "label": string | undefined,
  "container": Container,
  "jitter": number | undefined,
}
~~~

### broadcast {#field-broadcast}
By default a rendition's track lives in the same broadcast that served the catalog.
The `broadcast` field overrides that, naming a different broadcast that publishes the track.

The value is a relative path, resolved against the path of the broadcast that served the catalog.
It uses relative reference resolution ({{!RFC3986, Section 5.2}}): a non-empty reference replaces the catalog broadcast's last path segment before applying `.` and `..` segments.
For example, `./source` in a catalog served by `room/transcode` resolves to `room/source`, while `.` resolves to `room`.
An empty reference resolves to the catalog broadcast itself.
A publisher MUST NOT use an absolute path, nor a reference that escapes above the root.
The root is the consumer's authorized subtree, so such a reference names content the consumer cannot reach.
A consumer MUST reject a catalog containing one, rather than resolving the reference against a different broadcast or ignoring the rendition.

This lets a publisher author a catalog that points at tracks it does not republish.
For example, a transcoder produces a catalog listing its own downstream renditions alongside the untouched source rendition, referencing the latter in the source broadcast rather than copying the bytes through.

A consumer subscribes to such a rendition in the referenced broadcast, using the rendition's track name unchanged.

### label {#field-label}
The `label` field is a human-readable rendition name for a track picker.
It is presentation metadata, not the track name used to subscribe.
Multiple renditions MAY use the same label.

### container {#field-container}
The container used to frame this rendition's media, as described in {{container}}.
If absent, it defaults to `{ "kind": "legacy" }`.

### jitter {#field-jitter}
The maximum delay, in milliseconds, between a frame being ready and the publisher flushing it.
A consumer's jitter buffer SHOULD be at least this large to avoid stalling.
If absent, a consumer SHOULD assume each frame is flushed immediately.

It is measured at the publisher: how far behind the media clock a frame is when the publisher hands it to the transport, whether an encoder, a reorder buffer, or a segmenter held it.
An importer can estimate this delay from the media span of a batch, such as a run of audio PES packets between video packets in TS or an fMP4 fragment, without measuring the time spent waiting for input.
It is never a measurement of the network, which a consumer observes for itself and which no two consumers of the same broadcast would agree on.

A publisher MUST round the value up to a whole number of milliseconds, so a consumer sizing a buffer against it is never handed a bound below the real one.
A publisher MUST NOT advertise `0`; a track that flushes each frame immediately omits the field instead.
A consumer receiving `0` SHOULD treat the field as absent, since it is a publisher rounding down rather than a claim of zero delay.
A publisher MUST NOT lower a previously advertised value, since a burst it emitted once it may emit again.

For example:

- If each frame is flushed immediately, a video track's `jitter` is `1000/framerate` rounded up: 34 at 30 fps.
- If up to 3 B-frames may be emitted in a row, it is `3 * 1000/framerate`.
- If frames are buffered into 2 second segments, it is `2000`.
- If frames are flushed several at a time, it is the media span of the whole burst, not of one frame.

An audio frame's duration is codec dependent.
AAC often uses 1024 samples per frame, so at 44100Hz an immediately-flushed track's `jitter` is 24.

# Container {#container}
Audio, video, and text tracks use a container to encapsulate the media payload.
A rendition declares its container via the `container` field of its catalog entry ({{common}}):

~~~
type Container =
  { "kind": "legacy" } |
  { "kind": "cmaf", "init": string } |
  { "kind": "loc" }
~~~

The `kind` field selects the framing; a consumer MUST ignore a rendition whose `kind` it does not recognize.
Every container shares the same group rules:

Each moq-lite group MUST start with a keyframe.
If the codec does not support delta frames (e.g. audio), a group MAY consist of multiple keyframes.
Otherwise, a group MUST consist of a single keyframe followed by zero or more delta frames.

An empty group declares a discontinuity between codec epochs.
A consumer MUST reset codec state before decoding the next non-empty group, including reapplying any codec startup delay or pre-skip.
This applies whether the resumed timestamps move backward or forward.

## legacy
The default, used when the `container` field is absent.

Each frame starts with a timestamp, a QUIC variable-length integer (62-bit max) encoded in microseconds.
The remainder of the payload is codec specific; see the WebCodecs specification for specifics.

A frame with an empty codec payload is an end marker, not media.
Its timestamp is the exclusive endpoint of the source media.
When a codec must receive additional packets to emit buffered source samples, the marker MUST precede those terminal packets.
A consumer MUST NOT submit the marker to the codec decoder, MUST decode the terminal packets, and MUST discard decoded samples at or after the endpoint.

For example, h.264 with no `description` field would be annex.b encoded, while h.264 with a `description` field would be AVCC encoded.
For a text track, the remainder is the cue in the track's declared `format` (for example a `WEBVTT` segment).

## cmaf
Each frame is a complete fragmented MP4 fragment (`moof`+`mdat`), carrying its own timestamps.
Audio samples MUST be marked as sync samples, including samples inside a group.
A sync sample does not declare an audio group boundary; the publisher chooses those boundaries.

The `init` field is the initialization segment (`ftyp`+`moov`) for the track, base64-encoded ({{!RFC4648, Section 4}}).
A consumer MUST feed `init` to the decoder before the first frame.

## loc
Each frame is a Low Overhead Container frame {{!I-D.ietf-moq-loc}}: a property block, carrying the timestamp among other properties, followed by the codec payload.


# Compression {#compression}
Some metadata tracks are compressed.

Compression is signalled per track, never inferred from the track's name.
A data track ({{data}}) declares it with the `compression` field ({{field-compression}}).
The two tracks this specification defines as always compressed, `catalog.json.z` ({{catalog}}) and the timeline track ({{timeline}}), are identified by their role instead.
The `.z` suffix on those names is a naming convention, and a consumer MUST NOT treat it as a signal.

Each group is one raw DEFLATE stream ({{!RFC1951}}), sync-flushed at each frame boundary.
Each frame is therefore a self-delimited, byte-aligned slice, while later frames compress against the earlier ones in the same group.
A consumer MUST decompress a group's frames in order, starting from the first.

A sync flush ends with the empty-block marker `0x00 0x00 0xff 0xff`.
A publisher MUST omit this trailing marker from each frame and a consumer MUST append it before decompressing, the same trick as permessage-deflate ({{!RFC7692, Section 7.2.1}}).


# Timeline {#timeline}
The timeline track is the broadcast's segment index.
MoQ groups carry only an opaque sequence number; the timestamps live inside the media frames.
The timeline republishes the broadcast's segmentation as metadata: one record per segment, mapping a span of content time to the group ranges that carry it on each media track.
A consumer can answer "which groups cover time T on track X" and "where is the live edge" from a few bytes per segment, without downloading media.
This is sufficient to render an HLS or DASH playlist, seek a VOD recording, or index an archive.

The timeline is optional.
There is one timeline per broadcast, because its purpose is that segments are aligned across the broadcast's tracks: segment N covers the same span of content time on every track, which is what HLS requires of switchable renditions.
A broadcast that does not need aligned segments simply omits it.

## Catalog Section {#timeline-catalog}
The catalog's root `timeline` field advertises the track:

~~~
type TimelineSchema = {
  "track": string,
  "timescale": number | undefined,
  "durationMax": number | undefined,
  "wall": number | undefined,
}
~~~

The `track` field names the MoQ track carrying the segment records.
The name `timeline.z` is RECOMMENDED; a consumer MUST use the advertised name rather than assuming it.

The `timescale` field is the units per second for the records' `pts` and `duration` values, and for `durationMax` and `wall`.
If absent, it defaults to 1000 (milliseconds).

The `durationMax` field, if present, is the declared upper bound on a segment's `duration`, in `timescale` units.
A publisher that controls its encoder knows its keyframe cadence up front, so a consumer can size buffers or write an HLS `EXT-X-TARGETDURATION` from the catalog alone, before observing a single segment.
The value MUST NOT change for the life of the broadcast, and a publisher MUST NOT emit a record whose `duration` exceeds it.
A publisher that cannot honor that MUST omit the field rather than emit a record contradicting it.

The field is absent when the media decides the segmentation instead, which is the common case: a real-time encoder places keyframes on demand and a single GOP may be minutes long, and a publisher importing a source it does not control cannot promise anything about that source.
A consumer needing a bound then derives one from the records it has seen, raising it as longer segments arrive.

The `wall` field, if known, is the wall-clock time of `pts` 0: in `timescale` units, measured from the moq epoch, 2020-01-01T00:00:00Z.
A consumer derives the wall-clock time of any segment as `wall + pts`, and Unix time by adding the epoch back (for HLS `EXT-X-PROGRAM-DATE-TIME` or DASH `availabilityStartTime`).
The epoch is 2020 rather than 1970 so the value stays small, safely within a 53-bit integer even at fine timescales.

## Track Framing {#timeline-framing}
The timeline track is a sliding window of records.
An unbounded publisher only appends records, while a DVR publisher also removes records from the front as they expire.

The first frame of each group is a UTF-8 JSON object containing a checkpoint of the retained window:

~~~
{
  "offset": number,
  "start"?: number,
  "records": TimelineRecord[],
}
~~~

The `offset` is the absolute index of the oldest retained record.
`start` is the absolute index of `records[0]` and defaults to `offset` when omitted.
A publisher MAY omit a retained prefix from a checkpoint; a consumer that did not receive that prefix reports `offset` through `start` as skipped.
Each subsequent frame is one UTF-8 JSON operation, either `{ "push": TimelineRecord }` to append a record at the next absolute index, or `{ "pop": number }` to remove that many records from the front.
Indices MUST NOT exceed 2^53 - 1.

A publisher MAY roll groups to bound late-join cost or improve compression.
Each new group restates a decodable suffix of the retained window, so group boundaries are an encoding detail and MUST NOT surface as duplicate records to the application.
A consumer that missed records reports their absolute index range as skipped before continuing with the retained suffix.

The frames are DEFLATE-compressed ({{!RFC1951}}) within each group.
The publisher ends each frame's compressed data with an empty sync-flush block (the `0x00 0x00 0xff 0xff` trailer is removed, as in {{?RFC7692}}), so a consumer decompresses frames incrementally from the group's first frame.
The `.z` suffix on the RECOMMENDED track name marks this compression, mirroring the catalog's `catalog.json.z` sibling.

## Records {#timeline-records}
Each record describes one complete segment:

~~~
type TimelineRecord = {
  "segment": number,
  "pts": number,
  "duration": number,
  "tracks": Map<TrackName, TimelineRange[]> | undefined,
}

type TimelineRange = {
  "start": number,
  "end": number,
  "keyframe": boolean | undefined,
}
~~~

The `segment` field is the segment's number.
Numbers are consecutive within a broadcast, anchoring HLS `EXT-X-MEDIA-SEQUENCE`; they are explicit rather than implied by record order so a reader joining mid-stream, or reading a windowed recording, keeps stable numbering.

The `pts` field is the segment's start and `duration` its length, both in the timeline's timescale.
The next record's `pts` equals `pts + duration` unless content time itself jumped; a consumer SHOULD treat such a jump as a discontinuity.

The `tracks` field maps each participating track name to the group ranges it contributes.
Each range covers groups `start` through `end` inclusive, as used by moq-lite FETCH and SUBSCRIBE.
More than one range means the group sequence is discontinuous inside the segment: the skipped groups never existed.
A track absent from the map has no content for the span (a gap; HLS `EXT-X-GAP`).

Participating tracks need not be audio or video.
A catalog, or an application's own metadata track such as a chat log, is listed exactly like a media track, which is what lets a recording ({{recording}}) address all of them the same way.
A consumer that only wants renditions therefore MUST select tracks by consulting the catalog rather than by assuming every name in the map is media.
A record MUST tolerate and SHOULD preserve unknown fields, like the catalog.

The `keyframe` field states whether the range's first group starts with a keyframe, i.e. whether a player can join or switch renditions there.
If absent, it defaults to true; a publisher sets `false` when a source resumes without one, so an exporter knows not to advertise the segment as independently decodable.

## Segmentation {#timeline-segmentation}
A segment is a span of content time shared by every media track.
A track contributes every group whose start falls inside the span, so a segment boundary SHOULD land on a group start: every group already begins with a keyframe ({{container}}), so a boundary at a group start lets each track contribute whole groups and remain independently decodable.
A segment MAY span multiple groups of a track (short groups packed into a longer segment).

How boundaries are chosen is publisher policy: following a source's existing segmentation (an imported HLS playlist, CMAF segments on disk), or pacing by a minimum duration.
A publisher pacing itself SHOULD end a segment at the earliest point that is a group start on every enrolled track and at least the minimum past the segment's start, which makes the track with the coarsest groups pace the broadcast and leaves no track's group split across a boundary.
A minimum is always satisfiable, whereas a maximum is not: a single group longer than it cannot be divided.
Where no such point exists because two tracks have different coarse cadences, a publisher MUST choose one of them rather than a point interior to any track's group.

Whatever the policy, a publisher MUST NOT emit a record until the segment is complete: every *pacing* track's groups for the span are known.
Records are therefore self-contained and immediately servable, and the newest record is the live edge.
A pacing track that has produced nothing for the span holds the record back; a publisher that knows a track has stopped for good closes it, and the record then simply omits it (a gap).

A pacing track is one whose groups arrive continuously, which is what makes them usable as boundaries.
A track that publishes on its own schedule cannot pace: a catalog emits a group only when the renditions change, so a timeline waiting for it would stall the moment it went quiet.
Such a track is *non-pacing*: its groups are listed in whichever segment is open when they arrive, but it never determines a boundary and never holds a record back.
A publisher SHOULD record its catalog this way, so a recording can resolve the renditions in effect at any segment.

Placement of a non-pacing track's groups is therefore by arrival rather than by content time: nothing waits for them, so a group that arrives after its segment has already been published is listed in the next one.
When a segment closes, every non-pacing group that arrived while it was open belongs to that segment regardless of the timestamp basis carried by the non-pacing track.
The frames still carry their own timestamps, so no timing information is lost.
A non-pacing track's timestamps do not extend the final segment's duration.
A non-pacing track whose group never closes (an append-log such as a `moq-json` stream) is listed once, in the segment its group opened in.
A publisher that needs such content addressable per segment SHOULD roll the group at segment boundaries, which costs the shared compression window but makes each segment self-contained.

A group that starts before the first boundary belongs to the first segment.
The final segment of an ended broadcast has no closing boundary; its `duration` runs to the newest known content.
A publisher SHOULD carry the end of the last group's content into that value, since a publisher that knows only where each group *started* would report a duration one group short, and zero for a final segment that is a single group.


# MPEG-TS Service Information {#mpegts-si}
A broadcast imported from an MPEG-TS multiplex can carry the multiplex's standalone service-information tables (ISO/IEC 13818-1 program-specific information and ETSI EN 300 468 DVB SI: SDT, NIT, BAT, EIT, and any table the publisher does not recognize) so an exporter can re-emit them byte-for-byte.
The sections are opaque: nothing in this mechanism parses a table, so an unrecognized long-form table round-trips exactly like a known one.
Short-form tables, recognized or not, are carried with latest-value semantics instead (see below): the short-form tables broadcast systems define are clocks and stuffing, and a hypothetical multi-section static one would collapse to its most recent section.

The tables are state, not events: a transport stream retransmits them continuously only because a TS receiver can tune in at any moment, so the repetition rate is a property of the unreliable transport, not of the data.
Here each table rides a dedicated snapshot track, delivered once at join and republished on change, per the root catalog's guidance that dynamic content is delegated to other tracks.
Consumers that are not TS exporters never pay for it.

## Catalog Section {#mpegts-si-catalog}
The catalog's `mpegts` section maps each carried table to its track under an `si` field, keyed by the PID the sections ride on and then by `table_id`:

~~~
type Mpegts = {
  // ... other MPEG-TS carriage fields ...
  "si": { [pid: string]: { [tableId: string]: SiEntry } } | undefined,
}

type SiEntry = {
  "track": string,
  "interval": number | undefined,
}
~~~

Both keys are written in decimal, since JSON object keys are strings.
`table_id` is byte 0 of generic section syntax, so the key is no less generic than the PID; which table_id ranges mean what is a DVB convention that appears nowhere in the schema.

* `track`: the name of the snapshot track carrying this table's sections, on the same broadcast as the catalog.
* `interval`: how often an exporter MUST re-emit the sections at most, in milliseconds. A hint carrying the table's own repetition requirement (for DVB, the ETSI TS 101 211 maxima). When absent, an exporter SHOULD fall back to a fast cadence of its own choosing, so an unknown table degrades to a safe rate rather than being dropped.

Entries are added when a table is first observed, which is acquisition-time traffic; steady-state table revisions touch only the named track, never the catalog.

## Track Format {#mpegts-si-track}
An SI track is binary, without the media container framing ({{container}}).

Each frame is one sub-table: its complete current sections, concatenated verbatim in `section_number` order, each including its header and CRC.
Sections are self-delimiting via `section_length`, so the frame needs no framing of its own.
A sub-table's identity is its generic section header (`table_id`, `table_id_extension`) extended by the documented disambiguators for two DVB table families: `original_network_id` (bytes 8..10) for SDT other (`table_id` 0x46), and `transport_stream_id` plus `original_network_id` (bytes 8..12) for EIT (0x4E..0x6F), whose generic identities are only unique within one network or transport stream.

Each group is a snapshot: reading its frames in order yields the table's complete current state, where a later frame replaces an earlier one with the same sub-table identity.
A joiner reads only the newest group and is current; older groups never need to be fetched.
A writer MAY append a frame to an open group to revise one sub-table incrementally; a writer that only ever cuts complete groups is trivially conformant.

A publisher MUST NOT mix versions within a sub-table's frame: sections of one version are buffered until the generation is judged complete and committed atomically.
Completeness is contiguity (all of `0..=last_section_number` present) or one observed full transmission cycle, whichever comes first: some tables number their sections sparsely (DVB EIT schedule skips unused numbers per segment), so a repeated section within the pending generation proves the cycle wrapped and the set is complete as transmitted.
A section lost before the cycle wraps is indistinguishable from a legitimately skipped number, so a committed set can transiently omit it until the next cycle re-supplies it; no section-counting receiver can do better.
A publisher SHOULD carry only sections whose `current_next_indicator` is set.

A section without a long-form header (short-form syntax: TDT/TOT and similar) has no extension, version, or numbering, so its table is a single latest-value slot: each arrival replaces the last, and a byte-identical repetition is not a change.
This is what makes a time table proxyable: each tick republishes a small snapshot, a joiner reads the newest, and staleness is bounded by the source's own repetition interval plus path latency, the same bound a receiver of the original multiplex has.
Carrying the source's time rather than synthesizing one keeps the clock consistent with the EPG (event times are expressed on the source's clock) and preserves TOT's local-time-offset descriptors, which are operator policy a re-multiplexer cannot invent.


# Recording {#recording}
A live broadcast is bounded history: moq-lite {{moql}} serves the present with SUBSCRIBE and the recent past with FETCH, both ending at the publisher's cache.
A *recording* is the persistent tier, writing a broadcast to a filesystem or object store so it can be served back long after the live session ended.

A recording is addressed by segment.
The timeline ({{timeline}}) names every segment and the groups that carry it.
Each track's object is addressed by its smallest and largest stored group IDs; the timeline supplies those bounds without reading media.
A reader that wants segment N of a track issues one whole-object GET, with no range header and no second request, which is what both an HLS or DASH origin and a player seeking a VOD recording need.

A recording covers the tracks the timeline describes, plus the catalog and the timeline itself.
A broadcast with no timeline has no segments and cannot be recorded this way.

## Layout {#recording-layout}
A recording is a set of objects under a common prefix:

~~~
<prefix>/<encoded-track>/.info
<prefix>/<encoded-track>/groups/<largest>.<smallest>
<prefix>/<encoded-timeline-track>/segments/<segment>
~~~

The common prefix is application-defined and is not interpreted by this format.

`<encoded-track>` is the track's UTF-8 name with every byte outside `A-Z a-z 0-9 _ -` percent-encoded as `%` followed by two uppercase hexadecimal digits.
For example, `catalog.json` becomes `catalog%2Ejson`.
An encoded name never contains `/` or begins with `.`, so a track cannot address anything outside the prefix.
`.info`, `groups`, and `segments` are reserved within a track directory.

`<smallest>` and `<largest>` are the inclusive first and last group sequence numbers in the object.
Recorded group and segment IDs MUST be integers in the range 0 through 9007199254740991 (2^53 - 1), so timeline JSON preserves them exactly.
Each filename field is written as exactly 19 decimal digits, padded with leading zeros, so lexical and numeric order agree.
Writers and readers MUST reject IDs outside this range, including reconstructed group IDs in the binary table.
A range-named object MUST contain at least one group, and its table's first and last sequences MUST match the filename.
For each track, object ranges MUST be nonoverlapping and strictly increasing in timeline segment order; gaps are allowed both within and between objects.
A track with no stored groups for a segment has no object or range in that record.
There are no empty index objects or duplicate copies addressed by segment number.

This layout applies to every recorded track, including the catalog, except the recording-owned timeline track identified by the catalog's `timeline` field ({{timeline-catalog}}).
The timeline uses `segments/<segment>` with the same binary envelope and at least one complete Window group per object.
`<segment>` is the committed timeline segment ID, encoded as 19 zero-padded decimal digits within the recording ID range.
After its first segment, the recording MUST commit consecutive segment IDs, including all-gap segments.
A writer MUST stop before allocating an ID beyond the limit; IDs never wrap.
Each track has its own objects so a reader can fetch one rendition without downloading the others.

## Track Objects {#recording-track}
`<encoded-track>/.info` is a UTF-8 JSON object containing the immutable properties needed to interpret and replay the track:

~~~
{
  "version": 1,
  "priority": 0,
  "timescale": 1000000
}
~~~

All three fields are required integers.
`version` identifies this recording format and MUST be 1.
`priority` and `timescale` have the meanings of moq-lite `TRACK_INFO` {{moql}}: `priority` is in the range 0 through 255, and `timescale` is in the range 1 through 9007199254740991, so JSON consumers can preserve it exactly.
A reader MUST preserve integer values exactly.
`Publisher Max Age` is not stored; a reader supplies its own serving policy.

The track object MUST be durable before its first segment object is stored, including for the timeline track.
It is immutable for the lifetime of the recording.
A reader MUST refuse an unknown version or invalid track properties.
On an existing `.info`, a writer MUST validate and compare the parsed `version`, `priority`, and `timescale` values; JSON whitespace and member order do not affect equality.
Different property values MUST fail enrollment, and the existing object MUST NOT be rewritten.

## Segment Objects {#recording-segments}
A segment object holds one track's complete groups for one segment:

~~~
Segment Object {
  Version (i) = 1
  Group Count (i)
  Group Table {
    Sequence Delta (i)
    Frame Count (i)
    Frame Table {
      Timestamp (i)
      Payload Offset (i)
      Payload Length (i)
    } ...
  } ...
  Payload Bytes (..)
}
~~~

Fields annotated `(i)` are variable-length integers using the QUIC encoding ({{!RFC9000}}, Section 16).
`Group Count` gives the number of group entries; each `Frame Count` gives the number of frame entries in that group.
The first `Sequence Delta` is the absolute group sequence number.
Each subsequent value is the group sequence number minus the previous sequence number minus one; zero therefore means the next consecutive group.
Groups MUST have strictly ascending sequence numbers; reconstruction MUST reject values exceeding the recording ID limit ({{recording-layout}}).
For range-named objects, the sequences MUST match the ranges in the timeline.
Frame entries appear in their original order within each group.
`Timestamp` is the frame's absolute timestamp in the track's `timescale` units, not a delta.
It MUST be in the range 0 through 9007199254740991 (2^53 - 1), so browser replay preserves it exactly; writers and readers MUST reject larger values.

`Payload Offset` is relative to the start of `Payload Bytes`, immediately after the last table entry; `Payload Length` is the frame's payload size in bytes.
Payloads are concatenated in table order, without gaps, overlap, or trailing bytes.
They are the original frame payloads, including any group-scoped compression, without moq-lite FRAME headers.
A reader can locate a group or a frame index directly from the table without parsing earlier payloads.

A decoder MUST validate the complete table before accessing any payload.
Counts and entries MUST fit in the retrieved object, and every offset and length MUST describe a range within its payload bytes, with overflow checked before arithmetic.
An unknown version or malformed table is a missing segment, not an invalid recording.

A segment object MUST contain whole groups.
Segmentation forbids a boundary inside a track's group ({{timeline-segmentation}}), so a reader never has to stitch a group across objects.
Segment objects are immutable once written and are parseable without the timeline.

The timeline uses the same envelope and stores only complete Window groups ({{timeline-framing}}) closed while committing that segment.
Its own groups are discovered by listing, not included in the record's `tracks`, avoiding a record that must index itself.
A reader replays their checkpoints and operations in group order, preserving group boundaries and their independent DEFLATE windows.
Neither timeline nor media objects are appended to or rewritten.
One writer owns a recording prefix. An existing segment object key MAY be reused only for identical object bytes; a conflicting create MUST fail.

## Writer Behavior {#recording-writer}
A writer subscribes to the broadcast and buffers the in-progress segment independently of the publisher or relay cache.
It writes a track's segment object once that track's groups for the span are known.
Per-track objects are written independently: a track whose content is complete does not wait for a slower one.
The recording contract MUST NOT depend on a relay retaining those groups while storage catches up.

A writer MUST make each track's `.info` and segment object durable before publishing the timeline record that references them.
The recording owns its timeline encoder so it can publish the ranges actually stored.

If a track's object cannot be made durable, the writer MUST omit only that track from the record's `tracks`; successfully stored tracks retain their ranges.
The segment number and timing are unchanged, including when every track is omitted.
The writer pushes the resulting record and every subsequent record through its own Window encoder.
It MUST NOT copy source frames whose Window position or group-local DEFLATE dictionary depends on a record it changed.

A writer MUST accept new groups for each track in strictly increasing sequence order and MUST refuse a duplicate or decreasing sequence; it does not reorder arrivals.
Groups already accepted MAY complete in any order; the writer buffers them and writes their table in sequence order.
An incomplete group omitted by an application-forced segment cut MUST NOT be inserted into a later object if that would overlap or precede an earlier object's range.
The application MAY force a cut or remove a stalled track; storage does not impose a timeout.
A writer MUST NOT insert objects for earlier segments after committing a later segment.

For segment N, the writer publishes the stored ranges, applies any retention pops, closes the timeline's current Window group, and stores the complete groups under the timeline track's `segments/N` key, with N encoded as specified in {{recording-layout}}.
It MUST make that object durable before committing the next segment or deleting expired objects.
If the timeline object cannot be made durable, the recording stops at the preceding durable timeline object.

On a clean end, the writer MUST flush the final partial segment, finish the timeline track, and make its final complete groups durable.
There is no completion marker; object listing alone does not distinguish a clean end from an interruption.

## Retention {#recording-retention}
A recording has one of two retention modes:

- An *archive* is unbounded and retains every complete segment until explicitly deleted.
- A *DVR* retains a configured duration of complete segments and expires the oldest whole segments as newer ones become durable.

An application offering DVR without an explicit retention value SHOULD default to at least 30 seconds.
The writer removes the oldest segment only when the remaining complete segments still cover the configured duration.
Retention is measured from timeline records, not from wall-clock arrival or relay cache state.

Trimming occurs only as part of committing a new segment, before that segment's timeline object is finalized ({{recording-writer}}).
A recording MUST NOT trim after its final segment is committed.
Expiration first pops expired records from the timeline window and makes the resulting complete timeline groups durable, then deletes the expired segments' objects.
The writer MUST retain the latest timeline object, even for an all-gap segment, and enough earlier timeline groups to recover the retained window from a checkpoint.
The index therefore never advertises media the retention process has already deleted, although media objects MAY temporarily outlive the index.
Relay cache eviction does not change the recording timeline.

## Bootstrap and Recovery {#recording-recovery}
A reader or restarting writer lists the timeline track's `segments/` prefix and replays its objects in numeric segment order, including retention operations, from a retained checkpoint.
The catalog supplies the timeline track's name ({{timeline-catalog}}).
The recovered records determine the committed track object keys; a missing or malformed referenced object MUST NOT be served.
Objects not referenced by the recovered timeline do not advertise content on their own.
Listing is not an atomic snapshot across tracks; absence from an earlier listing MUST NOT override a successful GET of an object referenced by a later durable timeline record.

After replaying segment N below the recording ID limit, a reader follows an active recording by GET of `segments/N+1`, without refreshing media listings.
At the limit there is no next key; this does not establish a clean recording end.
Not Found does not distinguish a pending commit, an expired DVR segment, or an interrupted recording.
A reader that has fallen behind retention MUST bootstrap again from a retained checkpoint.
Alternatively, a reader MAY list timeline keys after its last replayed key, consume all pages, sort the results, and replay them in order.
A continuation token is used only within that enumeration; the next refresh starts from the last replayed key.
A reader MUST NOT advance its replay cursor past a missing segment without recovering from a retained checkpoint.
Retention operations evict expired ranges from the reader's cached index; incremental listing alone does not report deletions.

## Reader Behavior {#recording-reader}
Reading segment N of track T is: take the minimum and maximum group IDs from T's ranges in record N, GET `groups/<largest>.<smallest>`, and parse the groups.
The table MUST contain exactly the group sequences advertised for that track in the record, including its gaps.
Nothing on that path reads media the consumer did not ask for, and nothing requires a second request.

A reader MAY serve moq-lite FETCH from a recording.
Given a group, a reader locates the candidate object from a cached filename range index or the timeline's track ranges, then uses the table for the group and any requested `frame_start` {{moql}}.
A filename listing requires no media GETs.
On a backend guaranteeing lexical listing order and exclusive offsets, listing after `groups/<requested-group>` (19 padded digits, without the dot) selects the first candidate with an upper bound at least the requested group.
A reader MUST check the lower bound and committed timeline membership before serving it; an internal gap is resolved by the candidate's table.
An unordered listing MUST be collected and sorted before selecting a candidate; taking its first result is insufficient.
The reader reconstructs moq-lite FRAME headers from the stored timestamps and lengths, preserving the original payload bytes.
A group absent from the recording is a normal FETCH failure.

A reader deriving a presentation-ordered format renders it from the timeline and transmuxes segment objects on demand.
Nothing derived needs to be stored: the playlist or manifest is a function of the timeline, and a media segment is a function of one recorded object.
An HLS segment URI can carry the track and both group bounds, allowing the handler to resolve the exact object without listing or a segment-ID index.
HLS sequence numbers remain timeline metadata and need not occur in object names.


# Security Considerations
A rendition's `broadcast` reference ({{field-broadcast}}) resolves against the consumer's root, which is the subtree it is authorized for.
Clamping a reference that escapes above that root would silently redirect the subscription to an unrelated broadcast, so a consumer rejects the catalog instead.

TODO Security

A consumer parsing a recording ({{recording}}) is parsing data at rest that it did not necessarily write.
It MUST validate the group/frame table against the bytes actually retrieved before allocating from its counts or accessing payloads ({{recording-segments}}), and treat a malformed object as a missing segment rather than letting it invalidate the recording.
It MUST reject a track object with a zero `timescale`.
Varint fields are subject to the same limits as moq-lite {{moql}}.
A recording inherits the confidentiality and integrity properties of the storage holding it; encryption at rest is transparent to the format and out of scope.


# IANA Considerations

This document has no IANA actions.


--- back

# Appendix A: Changelog

## moq-hang-03
- Clarified that CMAF audio samples are sync samples independently of publisher group boundaries.
- Clarified that container importers can estimate jitter from batch media spans without measuring input wait time.
- Specified the `jitter` field's computation: the publisher's own structure rather than the network, rounded up to whole milliseconds, never `0` (a consumer treats `0` as absent), and never lowered once advertised. The 30 fps and 44.1 kHz AAC examples became 34 and 24.
- Specified version 1 recording objects: JSON track properties and binary group/frame tables with ascending, delta-encoded group sequences.
- Addressed track objects by inclusive group bounds and timeline objects by consecutive segment IDs, with incremental discovery and per-track omission on storage failure.
- Restricted retention updates to segment commits and removed completion markers.
- Limited recorded group and segment IDs and frame timestamps to JSON-safe integers, including delta reconstruction.
- Compared existing track properties by parsed values rather than JSON serialization.

# Acknowledgments
{:numbered="false"}

TODO acknowledge.
