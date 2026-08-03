---
title: "QMux over WebSocket"
abbrev: "qmux-ws"
category: info

docname: draft-lcurley-qmux-websocket-latest
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
  qmux: I-D.ietf-quic-qmux
  RFC6455:

informative:
  RFC8446:
  RFC9220:
  moqt: I-D.ietf-moq-transport

--- abstract

QMux [qmux] is a polyfill that runs QUIC applications over an ordered, reliable byte-stream transport such as TCP with TLS.
This document defines a binding for QMux over WebSocket [RFC6455].
A WebSocket binding lets QUIC applications reach environments where UDP is blocked and where only an HTTP/WebSocket stack is available, including web browsers that lack WebTransport.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}

This document uses the terms QMux Record, QMux Frame, and transport parameter as defined in [qmux], and the terms WebSocket connection, message, frame, and subprotocol as defined in [RFC6455].


# Introduction
QMux [qmux] lets an application written against the QUIC stream and datagram API run over an ordered, reliable byte-stream transport.
It defines a binding over TCP and over TLS, but a WebSocket binding is out of scope for the QUIC working group charter.

A WebSocket binding is nevertheless useful: WebSocket [RFC6455] is available where UDP is blocked, in web browsers that lack WebTransport, and behind HTTP load balancers that cannot route raw TCP or QUIC.

This document specifies how to carry QMux over WebSocket: the message framing, the subprotocol negotiation used in place of TLS ALPN, keep-alive behavior, and datagrams.
All other QMux semantics (in-order STREAM frame delivery, stream identifiers, flow control, transport parameters, and connection close) apply unchanged from [qmux].

This binding is application agnostic: any QUIC application that can run over QMux can run over QMux over WebSocket.
Media over QUIC Transport [moqt] is the motivating use case, but nothing in this document is specific to it.


# WebSocket Binding Overview
A QMux-over-WebSocket connection is an ordinary WebSocket connection [RFC6455] whose binary messages carry QMux frames, taking the place of the underlying byte-stream transport in [qmux].

Both the QMux Record layer and the WebSocket message layer provide self-delimiting messages over a reliable, ordered byte stream.
The two layers are therefore collapsed: instead of prefixing each Record with its `Size`, the binding relies on the WebSocket message boundary to delimit it.


# Establishing a Connection
A client establishes a QMux-over-WebSocket connection by opening a WebSocket connection per [RFC6455] (the opening handshake over HTTP/1.1) or per [RFC9220] (the bootstrapping mechanism over HTTP/2 or HTTP/3).

The `ws` URI scheme is used over an unencrypted transport and the `wss` URI scheme is used over a TLS-encrypted transport.
Deployments SHOULD use `wss`; an application that expects a TLS transport when running natively over QUIC SHOULD require `wss` here.

How the underlying connection is authenticated and authorized is out of scope for this document, as it is for [qmux].


# Subprotocol Negotiation
QMux over TCP/TLS uses TLS ALPN [RFC8446] to agree on the application protocol.
WebSocket has no ALPN exchange, so this binding uses the WebSocket subprotocol negotiation of [RFC6455] Section 1.9 (the `Sec-WebSocket-Protocol` header) in its place, carrying the same application protocol identifier.

The subprotocol identifier is exactly the identifier the application would use as its ALPN over native QUIC, for example `moq-transport-18`.
It also determines the QMux wire-format version, so there is no separate QMux version negotiation (see {{versions}}).

A client offers one or more identifiers in the `Sec-WebSocket-Protocol` request header, in decreasing order of preference, one per protocol version it is willing to use.
A server selects at most one, in its own order of preference, and echoes it in the response header; if it supports none of the offered identifiers, it MUST fail the handshake.
A client MUST treat the absence of a `Sec-WebSocket-Protocol` response header, or a response value it did not offer, as a failed handshake per [RFC6455].


# Record Framing {#framing}
Each WebSocket binary message carries exactly one QMux Record's `Frames` field: one or more QMux frames concatenated, as defined in [qmux].
Because the WebSocket framing layer already delimits each message, the QMux Record `Size` field is redundant: it MUST NOT be transmitted and MUST NOT be expected by the receiver.

A QMux-over-WebSocket record is therefore:

~~~
WebSocket Binary Message {
  Frames (..),
}
~~~

The frames inside a message are encoded exactly as in [qmux], including the in-order STREAM frame requirement: for each QUIC stream, a sender MUST send that stream's payload in order, so a receiver can deliver payload to the application as it arrives without reassembly.

An endpoint MAY place multiple frames in a single binary message and MAY split a logical sequence of frames across multiple messages, subject to the constraint that each STREAM frame's payload bytes are delivered in order.
An empty binary message (zero frames) is permitted and carries no frames; a receiver MUST accept it and treat it as a no-op.

The maximum size of a binary message is bounded by the `max_record_size` transport parameter defined in [qmux] and by any WebSocket implementation limits.
An endpoint MUST NOT send a binary message whose payload exceeds the peer's advertised `max_record_size`, and MAY treat receipt of an oversized message as a connection error.


# WebSocket Message Types
This binding uses WebSocket message and control frames as follows:

- *Binary messages* carry QMux frames as defined in {{framing}}.
- *Text messages* MUST NOT be sent. A receiver MUST treat a text message as a connection error and close the WebSocket connection.
- *Close frames* terminate the connection as defined in {{close}}.
- *Ping and Pong frames* are used for keep-alive as defined in {{keepalive}} and are otherwise handled by the WebSocket layer; they carry no QMux frames.

The first QMux frame sent by each endpoint MUST be the `QX_TRANSPORT_PARAMETERS` frame, exactly as required by [qmux]; this binding does not change that requirement.


# QMux Version {#versions}
The QMux version is not signaled on the wire.
As with QMux over TLS, it is implied by the negotiated application protocol: each application protocol that runs over QMux specifies which QMux version each of its identifiers uses.
An application protocol used with this binding MUST select a QMux version that provides the Record layer (see {{framing}}), i.e. [qmux] or later.


# Keep-Alive and Idle Timeout {#keepalive}
A WebSocket connection has no built-in idle timeout: if the peer's host crashes without a TCP FIN, the local socket can remain "open" for hours.

To detect a dead peer, an endpoint SHOULD send WebSocket Ping frames [RFC6455] periodically and SHOULD close the connection if no WebSocket frame of any kind is received within a timeout, a small multiple of the ping interval.
Reasonable defaults are a 5-second ping interval and a 30-second timeout, but the values are a local policy decision.
Receipt of any WebSocket frame from the peer (binary, Ping, or Pong) resets the idle timer.

This keep-alive operates at the WebSocket layer and is separate from the QMux `max_idle_timeout` transport parameter and `QX_PING` frame, either of which an endpoint MAY also use.


# Datagrams {#datagrams}
QMux datagrams are supported.
They are negotiated and encoded exactly as in [qmux]: an endpoint advertises the datagram transport parameter and carries QMux DATAGRAM frames inside binary messages, like any other frame ({{framing}}).


# Connection Close {#close}
An endpoint terminates a QMux-over-WebSocket connection by sending a WebSocket Close frame [RFC6455] and then closing the underlying transport.

A QMux `CONNECTION_CLOSE` frame, if sent, conveys the QMux-level error code and reason and SHOULD be sent in a final binary message before the WebSocket Close frame.
Because the WebSocket layer provides its own connection close, there is no draining period: an endpoint MAY close immediately after sending its Close frame.

Receipt of a WebSocket Close frame, or loss of the underlying transport, terminates the QMux connection and all of its streams.


# Security Considerations
This binding inherits the security considerations of QMux [qmux], WebSocket [RFC6455], and, when `wss` is used, TLS [RFC8446].

Carrying QMux over WebSocket does not add or remove any QMux-level security property.
In particular, this binding provides no transport-layer confidentiality or integrity of its own; deployments that require those properties MUST use `wss` (WebSocket over TLS).

The keep-alive mechanism in {{keepalive}} causes an endpoint to send periodic Ping frames.
An endpoint SHOULD bound the rate at which it sends and responds to Ping/Pong frames to avoid amplification or resource-exhaustion concerns.

Because a server selects the subprotocol from a client-supplied list, a server MUST validate the selected identifier against its own supported set and MUST NOT echo an arbitrary client-supplied value, as required by [RFC6455].


# IANA Considerations
This document has no IANA actions.

This binding defines no subprotocol identifiers of its own: the `Sec-WebSocket-Protocol` value is the application protocol identifier, which is owned by the application protocol's specification (for example, [moqt]).
Whether those identifiers are registered in the WebSocket Subprotocol Name Registry [RFC6455] is therefore up to each application protocol, not this document.


--- back

# Acknowledgments
{:numbered="false"}

QMux is the work of the QUIC working group; this document only defines a WebSocket binding for it.
Thanks to the Media over QUIC working group for motivating a transport that works where UDP does not.
