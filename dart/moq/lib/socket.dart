/// Point-to-point socket endpoints and sessions.
library;

import 'package:moq_ffi/moq_ffi.dart';

/// Announces one hidden endpoint and dials or accepts sessions.
typedef Endpoint = MoqSocketEndpoint;

/// An incoming TCP dial, consumed by accepting or rejecting it.
typedef IncomingTcp = MoqSocketIncomingTcp;

/// An incoming track dial, consumed by accepting or rejecting it.
typedef IncomingTrack = MoqSocketIncomingTrack;

/// A TCP session with group producer and consumer halves.
typedef TcpStream = MoqSocketTcpStream;

/// A track session with producer and reliably subscribed consumer halves.
typedef TrackStream = MoqSocketTrackStream;
