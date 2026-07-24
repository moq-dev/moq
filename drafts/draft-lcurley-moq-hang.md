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

# Catalog
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
	// ... any custom fields ...
}
~~~

Additional fields MAY be added based on the application.
The catalog SHOULD be mostly static, delegating any dynamic content to other tracks.

For example, a `"chat"` section should include the name of a chat track, not individual chat messages.
This way catalog updates are rare and a client MAY choose to not subscribe.

This specification currently only defines audio and video tracks.

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
Any Uint8Array fields are hex-encoded as a string.

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
Any Uint8Array fields are hex-encoded as a string.

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

## Common Rendition Fields {#common}
Audio and video renditions share the following fields, extending the WebCodecs decoder config:

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
Audio and video tracks use a container to encapsulate the media payload.
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

## cmaf
Each frame is a complete fragmented MP4 fragment (`moof`+`mdat`), carrying its own timestamps.

The `init` field is the initialization segment (`ftyp`+`moov`) for the track, base64-encoded ({{!RFC4648, Section 4}}).
A consumer MUST feed `init` to the decoder before the first frame.

## loc
Each frame is a Low Overhead Container frame {{!I-D.ietf-moq-loc}}: a property block, carrying the timestamp among other properties, followed by the codec payload.


# Security Considerations
TODO Security


# IANA Considerations

This document has no IANA actions.


--- back

# Acknowledgments
{:numbered="false"}

TODO acknowledge.
