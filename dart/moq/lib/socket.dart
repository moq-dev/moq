/// Point-to-point socket endpoints and sessions.
library;

import 'package:moq_ffi/moq_ffi.dart';

/// A server, a client, or both: its first dial announces it as a client, and its
/// first accept as a server.
typedef Endpoint = MoqSocketEndpoint;

/// An incoming track dial, consumed by accepting or rejecting it.
typedef IncomingTrack = MoqSocketIncomingTrack;

/// A track session with producer and reliably subscribed consumer halves.
typedef TrackStream = MoqSocketTrackStream;
