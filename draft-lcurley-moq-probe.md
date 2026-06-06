---
title: "MoQ Probe Extension"
abbrev: "moq-probe"
category: info

docname: draft-lcurley-moq-probe-latest
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

This document defines a PROBE extension for MoQ Transport {{moqt}}.
A subscriber opens a bidirectional PROBE stream to request that the publisher pad the connection up to a target bitrate.
The publisher sends padding as defined in {{moqt}} Section 11.5 and periodically responds with the measured bitrate and an elapsed timestamp, enabling the subscriber to estimate the available bandwidth.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}


# Introduction
Bandwidth estimation is essential for adaptive bitrate media delivery.
A subscriber needs to know the available bandwidth in order to select appropriate media tracks and qualities.

## Application-Limited
Many MoQ applications are application-limited: the average bitrate of the media is less than the available bandwidth.
Most congestion control algorithms only grow the congestion window or bandwidth estimate when fully utilized.
This means the available bandwidth is often underestimated, and the subscriber has no way to know if it can safely switch to a higher quality track.

This is particularly problematic for adaptive bitrate (ABR) algorithms.
A viewer may get stuck at a low quality rendition indefinitely because the congestion window never grows to reflect the true link capacity.
If the viewer does attempt to switch to a higher rendition without first probing, they risk buffering — either because the congestion window has not been warmed up to support the higher bitrate, or because the network genuinely cannot sustain it.
Without probing, the subscriber cannot distinguish between these two cases.

{{moqt}} Section 3.7.2 suggests subscribing to additional tracks at low priority or sending padding to fill the congestion window during probing intervals.
However, this is difficult in practice because the subscriber does not know when probing is needed or by how much.
The congestion window and bandwidth estimate are internal to the sender's congestion controller and are not exposed to the application, let alone the remote peer.
The subscriber cannot distinguish between "the network has more capacity" and "the congestion controller is already fully utilizing the link".

## Hop-by-Hop
MoQ is designed to work end-to-end via relays.
Each hop may have different network conditions, so bandwidth estimation must be performed per-hop rather than end-to-end.
A subscriber needs to know the capacity of its immediate connection, not the capacity of the origin.

Using a wire-level extension ensures that PROBE measurements are scoped to a single hop.
A relay terminates the PROBE stream and does not forward it upstream, avoiding incorrect measurements that reflect intermediate link capacity.

## This Extension
{{moqt}} defines padding streams and datagrams (Section 11.5) for probing, but does not define a signaling mechanism for a subscriber to request padding or for a publisher to report measurements.
This extension fills that gap: the subscriber opens a PROBE stream to request that the publisher pad the connection to a target bitrate using {{moqt}} padding.
The publisher responds with periodic measurements, allowing the subscriber to adjust its subscriptions accordingly.


# Setup Negotiation
The PROBE extension is negotiated during the SETUP exchange as defined in {{moqt}} Section 10.3.
Each endpoint advertises the capabilities it supports when acting as a publisher (the responder on a PROBE stream).

An endpoint indicates support by including the following Setup Option:

~~~
PROBE Setup Option {
  Option Key (vi64) = 0x950BE
  Option Value Length (vi64)
  Level (vi64)
}
~~~

**Level**:
The publisher capability level the sender supports, where each level includes the one below it:

- `1` **Report**: The publisher can measure and periodically report its estimated bitrate via PROBE_RESPONSE.
- `2` **Increase**: The publisher can additionally send padding (or redundant data) to probe for bandwidth above its current sending rate, up to the subscriber's target.

The levels are nested rather than independent: probing for more bandwidth is meaningless without measuring it, so Increase always includes Report. Reporting the current bitrate is far simpler to implement, so a publisher may support Report without Increase.

A subscriber MUST consult the publisher's advertised Level before relying on a PROBE stream:

- If the publisher omitted the option (or sent `0`), the PROBE extension is unavailable. A subscriber MUST NOT open a PROBE stream; if it does, the publisher MUST close the session with a PROTOCOL_VIOLATION.
- At `Report`, the subscriber MAY open a PROBE stream to monitor the measured bitrate but MUST NOT expect padding above the current sending rate. It MUST set Target Bitrate to 0 and use an alternative if it needs to probe for additional bandwidth.
- At `Increase`, the subscriber MAY request a non-zero Target Bitrate and expect the publisher to actively probe up to it.


# PROBE Stream
The PROBE extension defines two new MOQT messages, PROBE_REQUEST and PROBE_RESPONSE, exchanged on a bidirectional request stream.
{{moqt}} no longer assigns dedicated bidirectional stream types; a request stream is identified by its first message type, and per {{moqt}} Section 3.3 a bidirectional stream MUST NOT begin with a message type the peer has not negotiated.
The PROBE Setup Option (see [Setup Negotiation](#setup-negotiation)) performs this negotiation.

A subscriber opens the stream with a PROBE_REQUEST message, which MUST be the first message on the stream.
The subscriber (stream opener) sends PROBE_REQUEST messages and the publisher (responder) sends PROBE_RESPONSE messages on the same stream.
An endpoint that receives a PROBE_REQUEST without having advertised the PROBE Setup Option MUST close the session with a PROTOCOL_VIOLATION.
Either endpoint MAY close or reset the stream at any time.

Both messages use the {{moqt}} Section 10 control-message framing: a `Type` identifying the message, a 16-bit `Length`, and the payload.


## PROBE_REQUEST
A subscriber sends a PROBE_REQUEST to indicate the target the publisher should attempt to reach.

~~~
PROBE_REQUEST Message {
  Type (vi64) = 0x950BE
  Length (16)
  Target Bitrate (vi64)
}
~~~

**Target Bitrate**:
The desired bitrate in kilobits per second.
The publisher SHOULD send padding as defined in {{moqt}} Section 11.5 to attempt to reach this rate, but only if it advertised the Increase capability (see [Setup Negotiation](#setup-negotiation)).
A subscriber MUST set this to 0 unless the publisher advertised Increase.
A value of 0 indicates no padding is needed; the publisher SHOULD only send media data but MUST continue sending PROBE_RESPONSE messages.
This is useful for passively monitoring the current bitrate without actively probing for more bandwidth.
Either endpoint MAY close or reset the stream to stop receiving updates entirely.

The subscriber MAY send multiple PROBE_REQUEST messages on the same stream.
Each new PROBE_REQUEST supersedes the previous one.
The publisher MUST use the most recently received target.


## PROBE_RESPONSE
The publisher periodically sends PROBE_RESPONSE messages containing the measured bitrate and the elapsed time since the last response.

~~~
PROBE_RESPONSE Message {
  Type (vi64) = 0x950BF
  Length (16)
  Measured Bitrate (vi64)
  Elapsed (vi64)
}
~~~

**Measured Bitrate**:
The estimated bitrate in kilobits per second.
How this value is computed is implementation-defined and depends on the congestion controller.
Pacing-based algorithms (e.g. BBR) can report the current pacing rate directly, while window-based algorithms (e.g. CUBIC, Reno) may want to smooth the estimate since the sending rate is inherently bursty.
This includes media, padding, and any other data sent by the publisher.

**Elapsed**:
The number of milliseconds since the previous PROBE_RESPONSE on this stream.
For the first PROBE_RESPONSE, this is the number of milliseconds since the corresponding PROBE_REQUEST was received.
This allows the subscriber to assess the freshness of the measurement and detect stale updates caused by network delays.

The publisher SHOULD send PROBE_RESPONSE messages at regular intervals while probing is active.
The interval is implementation-defined but a value between 100ms and 1000ms is RECOMMENDED.


# Padding
The publisher SHOULD send padding using the mechanisms defined in {{moqt}} Section 11.5 (padding streams and/or padding datagrams).

The publisher MUST NOT exceed the target with padding alone.
If media traffic already meets or exceeds the target, no additional padding is necessary.

The publisher MUST respect the QUIC congestion controller.
Padding that would cause the congestion window to be exceeded MUST NOT be sent.
The goal is to fill unused capacity, not to cause congestion.


# Security Considerations
A malicious subscriber could request an excessively high target to waste publisher resources or cause network congestion.
Implementations SHOULD enforce reasonable limits on the target and MAY ignore or cap requests that exceed these limits.

A publisher SHOULD rate-limit the amount of padding it sends to avoid being used as an amplification vector.

A publisher MAY rate-limit or ignore frequent PROBE_REQUEST messages to prevent flooding or oscillation.
Implementations SHOULD enforce a minimum inter-request interval for PROBE_REQUESTs from a given subscriber.


# IANA Considerations

This document requests the following registrations:

## MOQT Setup Options

This document requests a registration in the "MOQT Setup Options" registry ({{moqt}} Section 15.4), whose policy is Specification Required.
moq-transport defines no private-use range for Setup Options; extensions request a (provisional) codepoint.
A high, distinctive value is chosen to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions; it also avoids the greasing pattern (`0x7f * N + 0x9D`).
This is the same value as the PROBE_REQUEST message type below; Setup Options and Message Types are independent registries, so the shared value is unambiguous.

| Value   | Name  | Reference     |
|:--------|:------|:--------------|
| 0x950BE | PROBE | This Document |

## MOQT Message Types

This document requests registrations in the "MOQT Message Types" registry ({{moqt}} Section 15).
{{moqt}} replaced dedicated bidirectional stream types with request message types, so the PROBE stream is identified by its first message rather than a stream type.
High, distinctive values are chosen to avoid the low ranges reserved by {{moqt}} and to minimize collisions with provisional registrations by other extensions; they also avoid the greasing pattern (`0x7f * N + 0x9D`).

| Value   | Name           | Stream         | Reference     |
|:--------|:---------------|:---------------|:--------------|
| 0x950BE | PROBE_REQUEST  | Request, First | This Document |
| 0x950BF | PROBE_RESPONSE | Request        | This Document |


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
