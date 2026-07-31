---
title: "MoQ Object Timestamp Extension"
abbrev: "moq-timestamp"
category: info

docname: draft-lcurley-moq-timestamp-latest
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
  loc: I-D.ietf-moq-loc

informative:

--- abstract

This document specifies the transport-level use of the TIMESTAMP and TIMESCALE properties registered by {{loc}}, independent of the LOC container itself.
A track-level Timescale property establishes the units, and an object-level Timestamp property carries the presentation time of each object.
Exposing media time to the transport lets relays make consistent age-based decisions (e.g. dropping stale objects) without parsing the media container, and it remains consistent across hops regardless of buffering or jitter.
No new code points are requested: an endpoint implementing this document is on the wire indistinguishable from a LOC endpoint that carries only these two properties.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}


# Introduction
{{moqt}} treats object payloads as opaque: "the amount of time elapsed between publishing an Object in Group ID N and in a Group ID > N ... is not defined by this specification" ({{moqt}} Section 2.3.1), and timing is left to the application's container format.

This works for endpoints that parse the media, but not for relays.
A relay frequently needs a notion of *when* an object is meant to be presented:

- **Age-based dropping**: a relay serving a live, latency-sensitive subscription wants to drop objects that are too old to be useful, keeping the freshest content flowing under congestion. Without a timestamp it can only approximate age from wall-clock arrival time, which drifts across hops and is corrupted by buffering and jitter.
- **Consistent expiration across hops**: every relay on a path should make the same drop decision for the same object. A timestamp embedded in the object is identical at every hop; a wall-clock arrival time is not.
- **Synchronization hints**: a subscriber can align objects from multiple tracks (e.g. audio and video) using a shared media timeline without first decoding each container.

MoQ also demultiplexes media into many independent tracks — audio, video, captions, metadata, and more — so a timestamp is needed on nearly every track.
Re-implementing per-object timestamping inside each application's container format, for every track, is repetitive and error-prone; standardizing it at the transport lets one implementation serve every track and lets relays use it directly.

This extension exposes media time to the transport with two Key-Value-Pairs ({{moqt}} Section 2.5): a track-level **Timescale** and an object-level **Timestamp**.
The transport does not interpret the *meaning* of the timeline (it is still the application's clock); it only uses the timestamp for relative age comparisons.

Both properties are already registered by {{loc}}, which defines them for use inside the LOC container.
This document reuses those registrations verbatim, with the same code points and value encodings, and specifies what a *transport* does with them.
That reuse is deliberate: a timestamp is only useful to a relay if every publisher writes it the same way, so a second set of code points for the same concept would defeat the purpose.
An endpoint that implements this document interoperates with a LOC endpoint without negotiation, and an endpoint that implements both writes one copy of each property, not two.

These properties are self-describing and require no SETUP negotiation: a receiver that understands them uses them directly, and one that does not ignores them per {{moqt}}.
TIMESCALE is what opts a track in.
A track that carries it declares that its objects have media times and states the units; a track without it declares no timeline at all, and a receiver falls back to wall-clock arrival time for any age-based decision.

There is deliberately no default timescale.
A default would have to be guessed at exactly the moment the publisher said nothing, and a wrong guess is off by a factor of 1000 rather than detectably broken.
Making presence the signal also keeps this document from contradicting {{loc}}, which reads a bare Timestamp as microseconds: a track this document describes always carries its own units, so the two never interpret the same bytes differently.


# TIMESCALE Track Property
The TIMESCALE property opts a track into timestamps and establishes the units for every Timestamp on it.
It is a track-level Key-Value-Pair, carried with the track's properties (see {{moqt}} Section 2.5 and Section 12).
Because the value is a single integer, TIMESCALE uses an even Type so the value is a bare varint with no length prefix:

~~~
TIMESCALE Track Property {
  Type (vi64) = 0x08
  Value (vi64)  ; units per second
}
~~~

**Value**:
The number of timestamp units per second.
Common values include `1000` (milliseconds), `1000000` (microseconds), `48000` (a typical audio sample rate), and `90000` (the RTP video clock).
A value of `0` is invalid; a receiver MUST treat a track that declares it as carrying no timeline.

Absence is meaningful and is not an error.
A track with no TIMESCALE has no media timeline: a receiver MUST NOT infer units for it, and MUST use wall-clock arrival time for age-based decisions on that track, exactly as it would for an object with no Timestamp.
Publishing a timestamp is therefore a per-track decision the publisher states once, rather than something a receiver discovers by watching objects go by.

A publisher that emits Timestamps MUST send TIMESCALE, including when the units are ones a receiver might otherwise assume.
{{loc}} permits a bare Timestamp and reads it as microseconds; a receiver that also implements LOC MAY apply that interpretation to a track that omits TIMESCALE, and MUST NOT apply any other.

The Timescale is fixed for the lifetime of the track and MUST NOT change.
{{loc}} also registers TIMESCALE with Object scope, allowing a per-object override.
A receiver that implements both applies such an override to that object alone; it does not redefine the track's timeline, and a publisher following this document SHOULD NOT send one.

The track's properties are delivered in SUBSCRIBE_OK or TRACK_STATUS ({{moqt}} Section 12).
A receiver that begins receiving objects before it has them does not yet know whether the track is opted in, so it MUST fall back to wall-clock arrival time for any age-based decision until the properties arrive.


# TIMESTAMP Object Property
The TIMESTAMP property carries the presentation time of an object, in the track's Timescale.
It is an object-level Key-Value-Pair carried in the object's properties ({{moqt}} Section 2.5, 11.2.1.2).
It uses an even Type so the value is a bare varint:

~~~
TIMESTAMP Object Property {
  Type (vi64) = 0x10
  Value (vi64)  ; absolute presentation time, in Timescale units
}
~~~

**Value**:
The absolute presentation timestamp of the object, expressed in the track's Timescale.
Any value (including 0) is valid.

Key-Value-Pair types are themselves delta-encoded from the previous type as an unsigned varint ({{moqt}} Section 1.4.3), so an object's properties are serialized in ascending type order.
TIMESTAMP (0x10) therefore follows any lower-numbered property in the same block, including an object-scope TIMESCALE (0x08).

Each Timestamp is absolute, not delta-encoded against a previous object.
{{moqt}} does not guarantee reliable delivery of every object within a group or subgroup, so an object may be dropped or lost independently; an absolute timestamp remains correct regardless, whereas a delta would be corrupted by any missing predecessor.

On a track that declares a TIMESCALE, a publisher SHOULD attach TIMESTAMP to every object that has a media time.
An object with no TIMESTAMP has no media time; for age comparisons a receiver MUST treat its effective time as the wall-clock arrival time of the object, which avoids stalling expiration on objects that intentionally carry no timestamp (e.g. keep-alives or gap markers).
The same fallback covers every object on a track that declares no TIMESCALE, so a receiver needs one rule, not two.

## Age-Based Dropping
Given two objects on the same track, both with TIMESTAMP, a relay computes their relative age as the difference of their timestamps divided by the Timescale.
A relay serving a live subscription MAY drop an object whose age relative to the most recent object on the track exceeds a locally configured or application-supplied threshold, resetting the corresponding stream per {{moqt}}.
This decision is identical at every hop because it depends only on values embedded in the objects, not on arrival time.

A relay MUST NOT use timestamps to reorder delivery beyond what {{moqt}} already permits; this property informs *dropping*, not transmission order.


# Security Considerations
Timestamps expose the media timeline to relays, which is the point of the extension, but a relay still treats payloads as opaque and gains no access to media content.

A malicious publisher could supply misleading timestamps (e.g. always claiming an object is fresh) to defeat age-based dropping, or wildly out-of-range timestamps to cause a receiver to mis-estimate age.
A receiver SHOULD bound the age it computes and SHOULD NOT make security decisions based on timestamps.
Because age-based dropping only affects which objects a live subscription receives, the worst case is degraded delivery for that subscription, not a cross-subscription effect.


# IANA Considerations

This document requests no registrations.

Both properties it uses are already registered by {{loc}} in the "MOQ Properties" registry ({{moqt}} Section 15.8), and this document changes neither their code points nor their value encodings:

| Value | Name      | Scope         | Reference |
|:------|:----------|:--------------|:----------|
| 0x08  | TIMESCALE | Track, Object | {{loc}}   |
| 0x10  | TIMESTAMP | Object        | {{loc}}   |

Both Types are even, so each value is a bare varint with no length prefix (see {{moqt}} Section 2.5).

An earlier version of this document requested its own code points (`0x915C0` and `0x915C2`) for these properties.
They are abandoned: two registrations for one concept would leave a relay unable to read half the traffic it is meant to make decisions about.


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
