package dev.moq.socket

/** Announces one hidden endpoint and dials or accepts sessions. */
typealias Endpoint = uniffi.moq.MoqSocketEndpoint

/** An incoming TCP dial, consumed by accepting or rejecting it. */
typealias IncomingTcp = uniffi.moq.MoqSocketIncomingTcp

/** An incoming track dial, consumed by accepting or rejecting it. */
typealias IncomingTrack = uniffi.moq.MoqSocketIncomingTrack

/** A TCP session with group producer and consumer halves. */
typealias TcpStream = uniffi.moq.MoqSocketTcpStream

/** A track session with producer and reliably subscribed consumer halves. */
typealias TrackStream = uniffi.moq.MoqSocketTrackStream
