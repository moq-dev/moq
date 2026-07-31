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
  moqt: I-D.ietf-moq-transport
  webcodecs: WebCodecs

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
- **Track**: An series of Groups, each of which can be delivered and decoded *out-of-order*.
- **Group**: An series of Frames, each of which must be delivered and decoded *in-order*.
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

If a publisher no longer wants to participant, or is disconnected somehow, their presence will be unannounced.
Publishers and subscribers SHOULD terminate any subscriptions once a participant is unannounced.

~~~
ANNOUNCE suffix=alice.hang active=false
~~~

# Catalog {#catalog}
The catalog describes the available media tracks for a single participant.
It's a JSON document that extends the the W3C WebCodecs specification.

The catalog is published as a `catalog.json` track within the broadcast so it can be updated live as the participant's media tracks change.
A participant MAY forgo publishing a catalog if it does not wish to publish any media tracks now and in the future.

The catalog track consists of multiple groups, one for each update.
Each group contains a single frame with UTF-8 JSON.

A publisher MUST NOT write multiple frames to a group until a future specification includes a delta-encoding mechanism (via JSON Patch most likely).

## Root
The root of the catalog is a JSON document with the following schema:

~~~
type Catalog = {
	"audio": AudioSchema | undefined,
	"video": VideoSchema | undefined,
	"timeline": TimelineSchema | undefined,
	"text": TextSchema | undefined,
	// ... any custom fields ...
}
~~~

Additional fields MAY be added based on the application.
The catalog SHOULD be mostly static, delegating any dynamic content to other tracks.

For example, a `"chat"` section should include the name of a chat track, not individual chat messages.
This way catalog updates are rare and a client MAY choose to not subscribe.

This specification defines audio, video, and text media tracks, plus an optional timeline track ({{timeline}}) indexing their segments.

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

In addition to the WebCodecs fields, each rendition MAY carry the fields common to audio and video ({{common}}) plus:

~~~
type VideoDecoderConfigExtensions = {
	"displayAspectWidth": number | undefined,
	"displayAspectHeight": number | undefined,
}
~~~

`displayAspectWidth` and `displayAspectHeight` give the display aspect ratio of the media, stretching or shrinking the coded pixels.
A consumer that understands neither field MUST assume square pixels, a 1:1 ratio.
Both MUST be present together; a consumer that sees only one MUST ignore it.

For example:

~~~
{
	"renditions": {
		"720p": {
			"codec": "avc1.64001f",
			"container": { "kind": "legacy" },
			"codedWidth": 1280,
			"codedHeight": 720,
			"bitrate": 6000000,
			"framerate": 30.0,
			"jitter": 33
		},
		"480p": {
			"codec": "avc1.64001e",
			"container": { "kind": "legacy" },
			"codedWidth": 848,
			"codedHeight": 480,
			"bitrate": 2000000,
			"framerate": 30.0,
			"jitter": 33
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

In addition to the WebCodecs fields, each rendition MAY carry the fields common to audio and video ({{common}}).

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
	"label": string | undefined,
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
The `label` field is a human-readable name for a track picker, useful when `lang` alone is ambiguous (for example distinguishing subtitles from same-language captions).

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

## Binary Fields {#binary}
A decoder config field carrying raw bytes, notably `description` (an `AllowSharedBufferSource` in WebCodecs), is carried in the catalog as a hex string ({{!RFC4648, Section 8}}).
A publisher SHOULD emit lowercase hexadecimal characters and MUST NOT emit a `0x` prefix or any separators.
A consumer MUST accept either case.

Note that this differs from the `cmaf` container's `init` field ({{container}}), which is base64 ({{!RFC4648, Section 4}}).
Encoding a binary field as base64 is not reliably detectable: the alphabets overlap, so such a value is usually rejected but may instead decode to the wrong bytes.

## Common Rendition Fields {#common}
Audio, video, and text renditions share the following fields, extending the WebCodecs decoder config for audio and video:

~~~
type CommonExtensions = {
	"broadcast": string | undefined,
	"container": Container,
	"jitter": number | undefined,
}
~~~

### broadcast {#field-broadcast}
By default a rendition's track lives in the same broadcast that served the catalog.
The `broadcast` field overrides that, naming a different broadcast that publishes the track.

The value is a relative path, resolved against the path of the broadcast that served the catalog.
It uses the `.` and `..` semantics of a relative URL reference ({{!RFC3986, Section 5.2.4}}), for example `../source`.
A publisher MUST NOT use an absolute path, and a consumer MUST ignore a rendition whose `broadcast` escapes above the root.

This lets a publisher author a catalog that points at tracks it does not republish.
For example, a transcoder produces a catalog listing its own downstream renditions alongside the untouched source rendition, referencing the latter in the source broadcast rather than copying the bytes through.

A consumer subscribes to such a rendition in the referenced broadcast, using the rendition's track name unchanged.

### container {#field-container}
The container used to frame this rendition's media, as described in {{container}}.
If absent, it defaults to `{ "kind": "legacy" }`.

### jitter {#field-jitter}
The maximum delay, in milliseconds, between a frame being ready and the publisher flushing it.
A consumer's jitter buffer SHOULD be at least this large to avoid stalling.
If absent, a consumer SHOULD assume each frame is flushed immediately.

For example:

- If each frame is flushed immediately, a video track's `jitter` is `1000/framerate`.
- If up to 3 B-frames may be emitted in a row, it is `3 * 1000/framerate`.
- If frames are buffered into 2 second segments, it is `2000`.

An audio frame's duration is codec dependent.
AAC often uses 1024 samples per frame, so at 44100Hz an immediately-flushed track's `jitter` is 23.

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
If codec does not support delta frames (ex. audio), then a group MAY consist of multiple keyframes.
Otherwise, a group MUST consist of a single keyframe followed by zero or more delta frames.

## legacy
The default, used when the `container` field is absent.

Each frame starts with a timestamp, a QUIC variable-length integer (62-bit max) encoded in microseconds.
The remainder of the payload is codec specific; see the WebCodecs specification for specifics.

For example, h.264 with no `description` field would be annex.b encoded, while h.264 with a `description` field would be AVCC encoded.
For a text track, the remainder is the cue in the track's declared `format` (for example a `WEBVTT` segment).

## cmaf
Each frame is a complete fragmented MP4 fragment (`moof`+`mdat`), carrying its own timestamps.

The `init` field is the initialization segment (`ftyp`+`moov`) for the track, base64-encoded ({{!RFC4648, Section 4}}).
A consumer MUST feed `init` to the decoder before the first frame.

## loc
Each frame is a Low Overhead Container frame {{!I-D.ietf-moq-loc}}: a property block, carrying the timestamp among other properties, followed by the codec payload.


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
The timeline track is an append-log: a single group that is never rolled, with one record per frame, every record preserved in order.
Each record is a UTF-8 JSON object.

The frames are DEFLATE-compressed ({{!RFC1951}}) sharing a single compression window across the group, so each record compresses against all earlier ones.
The publisher ends each frame's compressed data with an empty sync-flush block (the `0x00 0x00 0xff 0xff` trailer is removed, as in {{?RFC7692}}), so a consumer decompresses frames incrementally with one shared window.
The `.z` suffix on the RECOMMENDED track name marks this compression, mirroring the catalog's `catalog.json.z` sibling.

A consumer MUST start reading from the group's first frame; the shared window makes a mid-group join undecodable.
The live group is therefore bounded history; deep history is served from a recording.

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

The `tracks` field maps each participating media track name to the group ranges it contributes.
Each range covers groups `start` through `end` inclusive, as used by moq-lite FETCH and SUBSCRIBE.
More than one range means the group sequence is discontinuous inside the segment: the skipped groups never existed.
A track absent from the map has no content for the span (a gap; HLS `EXT-X-GAP`).
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

Whatever the policy, a publisher MUST NOT emit a record until the segment is complete: every participating track's groups for the span are known.
Records are therefore self-contained and immediately servable, and the newest record is the live edge.
An enrolled track that has produced nothing for the span holds the record back; a publisher that knows a track has stopped for good closes it, and the record then simply omits it (a gap).

A group that starts before the first boundary belongs to the first segment.
The final segment of an ended broadcast has no closing boundary; its `duration` runs to the newest known content.
A publisher SHOULD carry the end of the last group's content into that value, since a publisher that knows only where each group *started* would report a duration one group short, and zero for a final segment that is a single group.


# Recording {#recording}
A live broadcast is bounded history: moq-lite {{moql}} serves the present with SUBSCRIBE and the recent past with FETCH, both ending at the publisher's cache.
A *recording* is the persistent tier, writing a broadcast to a filesystem or object store so it can be served back long after the live session ended.

A recording is addressed by segment.
The timeline ({{timeline}}) already names every segment and the groups that carry it, so it is the recording's only index: the segment number names the object, and no separate index needs to be stored, refreshed, or kept consistent with the media.
A reader that wants segment N of a track issues one whole-object GET, with no range header and no second request, which is what both an HLS or DASH origin and a player seeking a VOD recording need.

A recording covers the tracks the timeline describes, plus the catalog and the timeline itself.
A broadcast with no timeline has no segments and cannot be recorded this way.

## Layout {#recording-layout}
A recording is a set of objects under a common prefix:

~~~
<prefix>/.catalog/<segment>
<prefix>/.timeline
<prefix>/<track>/<segment>
~~~

`<track>` is the media track's name with every byte outside `A-Z a-z 0-9 _ -` percent-encoded.
An encoded name therefore never contains `/` and never begins with `.`, so a track can neither collide with the reserved `.catalog` and `.timeline` names nor address anything outside the prefix.

`<segment>` is the timeline record's `segment` value in decimal without leading zeros, so an object name is computed from a record rather than discovered by listing.

Each media track gets its own objects so a consumer can fetch one rendition without paying for the others, which is the point of publishing switchable renditions at all.
For the same reason a publisher SHOULD NOT combine tracks into a single object, even when they are always played together: doing so forces every consumer to fetch the highest rendition it does not want.

## Segment Objects {#recording-segments}
A segment object holds one track's content for one segment, as a sequence of groups:

~~~
Segment Object {
  Group {
    Sequence (i)
    Length (i)
    Frame (..) ...
  } ...
}
~~~

Fields annotated `(i)` are variable-length integers using the QUIC encoding ({{!RFC9000}}, Section 16).

**Sequence** is the group's sequence number, matching the range that listed it in the timeline record.
Groups appear in ascending sequence order.

**Length** is the byte length of the group's frames, so a reader skips a group it does not want without parsing it.

**Frame** is the moq-lite FRAME encoding {{moql}}: a zigzag `Timestamp Delta`, a `Message Length`, and the payload, exactly as the group was delivered.
A group's frames are therefore byte-identical to the body of a FETCH response for that group, and a recording never re-encodes media.

A segment object MUST contain whole groups.
Segmentation already forbids a boundary interior to any track's group ({{timeline-segmentation}}), so a group is never split across two segments and a reader never has to stitch one back together.

Because the object carries its own group boundaries it is parseable on its own, without the timeline.
The timeline is needed to *find* an object, not to read one.

Segment objects are immutable once written.

## Timeline and Catalog Objects {#recording-metadata}
`.timeline` holds the frames of the timeline track verbatim, in order, using the track's own framing ({{timeline-framing}}).
The object grows as the broadcast does, and its content is append-only: bytes once written never change.

A reader starting fresh reads the object from the beginning, which the shared DEFLATE window requires anyway.
A reader following a live recording issues a ranged GET from the offset it last read and feeds the new bytes to the decompressor it already holds: each frame's sync-flush block terminates its own compressed data, so the appended bytes decode without re-reading earlier ones.

`.catalog/<segment>` holds one catalog frame ({{catalog}}), where `<segment>` is the first segment it applies to.
The catalog in effect for segment N is the object with the highest number not greater than N, so a reader resolves the renditions available at any point in the recording without scanning.
A publisher that updates the catalog more than once within a single segment records only the last, since the earlier states were never in effect at a segment boundary.

## Writer Behavior {#recording-writer}
A writer subscribes to the broadcast, buffers each track's groups, and writes a track's segment object once that track's groups for the span are known.
Per-track objects are written independently: a track whose content is complete does not wait for a slower one.

A writer MUST make a segment's objects durable before appending the timeline record that references them, so a reader following `.timeline` never sees a record naming an object that does not yet exist.
Since the timeline record itself is only published once the segment is complete on every track ({{timeline-segmentation}}), this orders the whole recording: objects, then the record that indexes them.

A group that arrives after its segment object has been written is not recorded.
A writer SHOULD wait for the segment to be complete rather than write early, and MAY bound that wait so a track that has stopped without closing cannot stall the recording indefinitely.
This is a deliberate trade: addressing content by segment is what makes a segment retrievable in one request, and it costs the ability to append a late group to an object already written.

A writer SHOULD record the broadcast's final state on a clean end, so a reader can distinguish an ended recording from one whose writer died.
Once ended, every object in the recording is immutable.

## Reader Behavior {#recording-reader}
Reading segment N of track T is: resolve T's object name from N, GET it, and parse the groups.
Nothing on that path reads media the consumer did not ask for, and nothing requires a second request.

Segment objects are immutable and SHOULD be served with long-lived caching.
`.timeline` and `.catalog` change while the recording is live and SHOULD be served with short lifetimes; all three become immutable once the recording has ended.

A reader MAY serve moq-lite FETCH from a recording.
Given a group, the timeline record whose range covers it names the segment; the object's group headers locate the group within it; and its frames are the FETCH response body unchanged.
A FETCH bounded to a frame range {{moql}} is served by counting frames within that group.
A group absent from the recording is a normal FETCH failure.

A reader deriving a presentation-ordered format renders it from the timeline and transmuxes segment objects on demand.
Nothing derived needs to be stored: the playlist or manifest is a function of the timeline, and a media segment is a function of one recorded object.


# Security Considerations
TODO Security

A consumer parsing a recording ({{recording}}) is parsing data at rest that it did not necessarily write.
It MUST bounds-check a segment object's group `Length` and each frame's `Message Length` against the bytes actually retrieved, rather than trusting either to describe the object, and it MUST treat a malformed object as a missing segment rather than letting it invalidate the recording.
Varint fields are subject to the same limits as moq-lite {{moql}}.
A recording inherits the confidentiality and integrity properties of the storage holding it; encryption at rest is transparent to the format and out of scope.


# IANA Considerations

This document has no IANA actions.


--- back

# Acknowledgments
{:numbered="false"}

TODO acknowledge.
