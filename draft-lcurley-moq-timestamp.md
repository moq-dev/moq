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

informative:

--- abstract

This document defines an extension for MoQ Transport {{moqt}} that attaches a media presentation timestamp and duration to each object.
A track-level Timescale property establishes the units, an object-level Timestamp property carries the presentation time of each object, and an optional Duration property carries its presentation duration.
Exposing media time to the transport lets relays make consistent age-based decisions (e.g. dropping stale objects) without parsing the media container, and it remains consistent across hops regardless of buffering or jitter.

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

This extension exposes media time to the transport with three Key-Value-Pairs ({{moqt}} Section 2.5): a track-level **Timescale**, an object-level **Timestamp**, and an optional object-level **Duration**.
The transport does not interpret the *meaning* of the timeline (it is still the application's clock); it only uses the timestamp for relative age comparisons.


# Setup Negotiation
The Object Timestamp extension is negotiated during the SETUP exchange as defined in {{moqt}} Section 10.3.
An endpoint indicates support by including the following Setup Option:

~~~
TIMESTAMP Setup Option {
  Option Key (vi64) = 0x915C1
  Option Value Length (vi64) = 0
}
~~~

The properties defined below are ordinary Key-Value-Pairs and a receiver that does not understand them ignores them per {{moqt}}.
Negotiation is therefore not required for correctness, but a publisher SHOULD send the Setup Option so that a relay knows it can rely on object timestamps for age-based decisions rather than falling back to wall-clock arrival time.
A relay MAY perform timestamp-based dropping for a track only if the upstream publisher advertised this option (or the track carries a non-zero Timescale).


# TIMESCALE Track Property
The TIMESCALE property establishes the units for every Timestamp and Duration on a track.
It is a track-level Key-Value-Pair, carried with the track's properties (see {{moqt}} Section 2.5 and Section 12).
Because the value is a single integer, TIMESCALE uses an even Type so the value is a bare varint with no length prefix:

~~~
TIMESCALE Track Property {
  Type (vi64) = 0x915C0
  Value (vi64)  ; units per second
}
~~~

**Value**:
The number of timestamp units per second.
Common values include `1000` (milliseconds), `1000000` (microseconds), `48000` (a typical audio sample rate), and `90000` (the RTP video clock).
A value of `0`, or the absence of the property, means the track has no media timeline: Timestamp and Duration properties, if present, MUST be ignored, and a relay MUST fall back to wall-clock arrival time for any age-based decision.

The Timescale is fixed for the lifetime of the track and MUST NOT change.

The Timescale is required to interpret the units of every Timestamp and Duration, so a receiver cannot resolve an object's timing until it has the track's properties.
Those properties are delivered in SUBSCRIBE_OK or TRACK_STATUS ({{moqt}} Section 12), so a receiver that begins receiving objects before it has them MUST buffer the timing (or treat it as unknown) until the Timescale arrives.
A relay that has not yet learned the Timescale MUST fall back to wall-clock arrival time for any age-based decision.


# TIMESTAMP Object Property
The TIMESTAMP property carries the presentation time of an object, in the track's Timescale.
It is an object-level Key-Value-Pair carried in the object's properties ({{moqt}} Section 2.5, 11.2.1.2).
It uses an even Type so the value is a bare varint:

~~~
TIMESTAMP Object Property {
  Type (vi64) = 0x915C2
  Value (vi64)  ; absolute presentation time, in Timescale units
}
~~~

**Value**:
The absolute presentation timestamp of the object, expressed in the track's Timescale.
Any value (including 0) is valid.

A publisher SHOULD attach TIMESTAMP to every object on a track whose Timescale is non-zero.
An object with no TIMESTAMP on such a track has no media time; for age comparisons a receiver MUST treat its effective time as the wall-clock arrival time of the object, which avoids stalling expiration on objects that intentionally carry no timestamp (e.g. keep-alives or gap markers).

## Age-Based Dropping
Given two objects on the same track, both with TIMESTAMP and a non-zero Timescale, a relay computes their relative age as the difference of their timestamps divided by the Timescale.
A relay serving a live subscription MAY drop an object whose age relative to the most recent object on the track exceeds a locally configured or application-supplied threshold, resetting the corresponding stream per {{moqt}}.
This decision is identical at every hop because it depends only on values embedded in the objects, not on arrival time.

A relay MUST NOT use timestamps to reorder delivery beyond what {{moqt}} already permits; this property informs *dropping*, not transmission order.


# DURATION Object Property
The DURATION property carries the presentation duration of an object, in the track's Timescale.
It is optional and is an object-level Key-Value-Pair with an even Type:

~~~
DURATION Object Property {
  Type (vi64) = 0x915C4
  Value (vi64)  ; presentation duration, in Timescale units
}
~~~

**Value**:
The presentation duration of the object, expressed in the track's Timescale.
A value of `0`, or the absence of the property, means the duration is unknown; the object is presented until the next object begins.

Duration is primarily an application-level presentation hint, but a relay MAY also use it to refine age-based dropping: an object's Timestamp plus its Duration marks the end of its presentation interval, which is a more precise "this object is now in the past" signal than the Timestamp alone (for example, the last object of a group has no following object to bound it). A relay MUST NOT rely on Duration being present; when it is absent, the relay falls back to comparing Timestamps as in [Age-Based Dropping](#age-based-dropping).


# Security Considerations
Timestamps expose the media timeline to relays, which is the point of the extension, but a relay still treats payloads as opaque and gains no access to media content.

A malicious publisher could supply misleading timestamps (e.g. always claiming an object is fresh) to defeat age-based dropping, or wildly out-of-range timestamps to cause a receiver to mis-estimate age.
A receiver SHOULD bound the age it computes and SHOULD NOT make security decisions based on timestamps.
Because age-based dropping only affects which objects a live subscription receives, the worst case is degraded delivery for that subscription, not a cross-subscription effect.


# IANA Considerations

This document requests the following registrations.
High, distinctive values are requested to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions; they also avoid the greasing pattern (`0x7f * N + 0x9D`).
The three property Types are even so that each value is a bare varint with no length prefix (see {{moqt}} Section 2.5).

## MOQT Setup Options

This document requests a registration in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required.

| Value   | Name      | Reference     |
|:--------|:----------|:--------------|
| 0x915C1 | TIMESTAMP | This Document |

## MOQT Properties

This document requests registrations in the "MOQT Properties" registry ({{moqt}} Section 15.8), used for object and track properties.

| Value   | Name      | Scope  | Reference     |
|:--------|:----------|:-------|:--------------|
| 0x915C0 | TIMESCALE | Track  | This Document |
| 0x915C2 | TIMESTAMP | Object | This Document |
| 0x915C4 | DURATION  | Object | This Document |


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
