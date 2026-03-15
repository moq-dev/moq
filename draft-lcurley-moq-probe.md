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
A subscriber opens a bidirectional PROBE stream to request that the publisher pad the connection up to a target.
The publisher periodically responds with the measured bitrate and an elapsed timestamp, enabling the subscriber to estimate the available bandwidth.

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

{{moqt}} Section 3.7.2 suggests subscribing to additional tracks at low priority to fill the congestion window during probing intervals.
However, this is difficult in practice because the subscriber does not know when probing is needed or by how much.
The congestion window and bandwidth estimate are internal to the sender's congestion controller and are not exposed to the application, let alone the remote peer.
The subscriber cannot distinguish between "the network has more capacity" and "the congestion controller is already fully utilizing the link".
It also requires the publisher to have pre-encoded padding tracks and the subscriber to manage extra subscriptions.

## Hop-by-Hop
MoQ is designed to work end-to-end via relays.
Each hop may have different network conditions, so bandwidth estimation must be performed per-hop rather than end-to-end.
A subscriber needs to know the capacity of its immediate connection, not the capacity of the origin.

Using a wire-level extension ensures that PROBE measurements are scoped to a single hop.
A relay terminates the PROBE stream and does not forward it upstream, avoiding incorrect measurements that reflect intermediate link capacity.

## This Extension
This extension provides a simple mechanism for bandwidth estimation.
The subscriber opens a PROBE stream and requests that the publisher pad the connection to a target.
The publisher responds with periodic measurements, allowing the subscriber to adjust its subscriptions accordingly.


# Setup Negotiation
The PROBE extension is negotiated during the SETUP exchange as defined in {{moqt}} Section 9.4.

Both endpoints indicate support by including the following Setup Option:

~~~
PROBE Setup Option {
  Option Key (vi64) = 0xPROBE_TODO
  Option Value Length (vi64) = 0
}
~~~

If both endpoints include this option, the PROBE extension is available for the session.
If a peer receives a PROBE stream without having negotiated the extension, it MUST close the session with a PROTOCOL_VIOLATION.


# PROBE Stream
The PROBE extension uses a new bidirectional stream type.

~~~
STREAM_TYPE = 0xPROBE_TODO
~~~

The stream type is sent at the beginning of the stream, encoded as a variable-length integer, consistent with {{moqt}} stream type framing.

A subscriber (stream opener) sends PROBE_REQUEST messages on the stream.
The publisher (responder) sends PROBE_RESPONSE messages on the stream.
Either endpoint MAY close or reset the stream at any time.


## PROBE_REQUEST
A subscriber sends a PROBE_REQUEST to indicate the target the publisher should attempt to reach.

~~~
PROBE_REQUEST {
  Message Length (vi64)
  Target Bitrate (vi64)
}
~~~

**Target Bitrate**:
The desired bitrate in kilobits per second.
The publisher SHOULD pad the connection to attempt to reach this rate.
A value of 0 indicates no padding is needed; the publisher SHOULD only send media data but MUST continue sending PROBE_RESPONSE messages.
This is useful for passively monitoring the current bitrate without actively probing for more bandwidth.
Either endpoint MAY close or reset the stream to stop receiving updates entirely.

The subscriber MAY send multiple PROBE_REQUEST messages on the same stream.
Each new PROBE_REQUEST supersedes the previous one.
The publisher MUST use the most recently received target.


## PROBE_RESPONSE
The publisher periodically sends PROBE_RESPONSE messages containing the measured bitrate and the elapsed time since the last response.

~~~
PROBE_RESPONSE {
  Message Length (vi64)
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
Padding is optional and depends on the capabilities of the QUIC implementation.
A publisher that does not support padding MUST still send PROBE_RESPONSE messages based on the actual sending rate.

## QUIC-Level Padding
The preferred method is for the QUIC implementation to send PING+PADDING frames.
PADDING frames alone MUST NOT be used, as they are not ack-eliciting and can cause starvation of the congestion controller.
PING+PADDING is transparent to the application and does not consume application-level flow control.

## Datagram Padding
If the QUIC implementation does not expose a padding API, the publisher MAY send QUIC datagrams as a fallback.
Datagrams are unreliable and do not consume stream-level flow control, making them suitable for padding.

A PROBE datagram is identified by a well-known datagram type:

~~~
PROBE Datagram {
  Datagram Type (vi64) = 0xPROBE_TODO
  Padding (..)
}
~~~

The contents of the Padding field are arbitrary and MUST be discarded by the receiver.
The receiver MUST NOT interpret the contents as application data.

## General Requirements
Padding SHOULD be sent at the lowest priority to avoid interfering with media delivery.

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

## MOQT Setup Option Type

This document registers the following entry in the "MoQ Setup Option Types" registry:

| Value | Name | Reference |
|:------|:-----|:----------|
| 0xPROBE_TODO | PROBE | This Document |

## MOQT Stream Type

This document registers the following entry in the "MoQ Stream Types" registry:

| Value | Name | Reference |
|:------|:-----|:----------|
| 0xPROBE_TODO | PROBE | This Document |

## MOQT Datagram Type

This document registers the following entry in the "MoQ Datagram Types" registry:

| Value | Name | Reference |
|:------|:-----|:----------|
| 0xPROBE_TODO | PROBE | This Document |


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
