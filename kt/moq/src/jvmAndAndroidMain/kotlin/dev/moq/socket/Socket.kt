package dev.moq.socket

/**
 * A server, a client, or both: its first dial announces it as a client, and its first accept as a
 * server.
 */
typealias Endpoint = uniffi.moq.MoqSocketEndpoint

/** An incoming track dial, consumed by accepting or rejecting it. */
typealias IncomingTrack = uniffi.moq.MoqSocketIncomingTrack

/** Whether [path] is an endpoint's broadcast, which only routes sessions and carries no media. */
fun isEndpoint(path: String): Boolean = uniffi.moq.moqSocketIsEndpoint(path)

/** A track session with producer and reliably subscribed consumer halves. */
typealias TrackStream = uniffi.moq.MoqSocketTrackStream
