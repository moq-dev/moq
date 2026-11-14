package dev.moq.socket

/**
 * A server, a client, or both: its first dial announces it as a client, and its first accept as a
 * server.
 */
typealias Endpoint = uniffi.moq.MoqSocketEndpoint

/** An incoming track dial, consumed by accepting or rejecting it. */
typealias IncomingTrack = uniffi.moq.MoqSocketIncomingTrack

/** A track session with producer and reliably subscribed consumer halves. */
typealias TrackStream = uniffi.moq.MoqSocketTrackStream
