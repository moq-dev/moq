package dev.moq.socket

/**
 * A server, a client, or both: its first dial announces it as a client, and its first accept as a
 * server.
 */
typealias Endpoint = uniffi.moq.MoqSocketEndpoint

/** An incoming TCP dial, consumed by accepting or rejecting it. */
typealias IncomingTcp = uniffi.moq.MoqSocketIncomingTcp

/** An incoming track dial, consumed by accepting or rejecting it. */
typealias IncomingTrack = uniffi.moq.MoqSocketIncomingTrack

/** A TCP session with group producer and consumer halves. */
typealias TcpStream = uniffi.moq.MoqSocketTcpStream

/** A track session with producer and reliably subscribed consumer halves. */
typealias TrackStream = uniffi.moq.MoqSocketTrackStream
