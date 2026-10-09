package dev.moq

import kotlin.time.Duration
import kotlin.time.Duration.Companion.microseconds

// The FFI surface carries microseconds as plain integers, because not every
// target language has a duration type. Kotlin does, so read them back as one.
// These are extension properties on the typealiased records, so the `*Us`
// fields stay available for anyone building a record to send back across the
// boundary.

/** Smoothed round-trip time, or null when the transport has no estimate yet. */
val ConnectionStats.rtt: Duration?
    get() = rttUs?.toLong()?.microseconds

/** Delay before the first reconnect attempt, or null for the default. */
val Backoff.initial: Duration?
    get() = initialUs?.toLong()?.microseconds

/** Maximum delay between reconnect attempts, or null for the default. */
val Backoff.max: Duration?
    get() = maxUs?.toLong()?.microseconds

/** Time spent retrying before giving up, or null for the default. [Duration.ZERO] retries forever. */
val Backoff.timeout: Duration?
    get() = timeoutUs?.toLong()?.microseconds

/** Head start QUIC gets before the WebSocket fallback joins, or null for the default. */
val WebSocketConfig.delay: Duration?
    get() = delayUs?.toLong()?.microseconds

/** Upper bound on buffering before a stalled group is skipped. */
val Subscription.maxDelay: Duration
    get() = maxDelayUs.toLong().microseconds

/** Maximum age of a non-latest group before the publisher evicts it, or null for the default. */
val TrackInfo.maxAge: Duration?
    get() = maxAgeUs?.toLong()?.microseconds

/** Upper bound on buffering before a stalled group is skipped, or null for the default. */
val AudioDecoderOutput.maxDelay: Duration?
    get() = maxDelayUs?.toLong()?.microseconds

/** Upper bound on buffering before a stalled group is skipped, or null for the default. */
val VideoDecoderOutput.maxDelay: Duration?
    get() = maxDelayUs?.toLong()?.microseconds

/** Encoded frame duration. */
val AudioEncoderOutput.frameDuration: Duration
    get() = frameDurationUs.toLong().microseconds

/** Presentation timestamp, or null for an untimed frame. */
val Frame.timestamp: Duration?
    get() = timestampUs?.toLong()?.microseconds

/** Presentation timestamp, or null for an untimed datagram. */
val Datagram.timestamp: Duration?
    get() = timestampUs?.toLong()?.microseconds

/** Presentation timestamp of the first sample. */
val AudioFrame.timestamp: Duration
    get() = timestampUs.toLong().microseconds

/** Presentation timestamp. */
val VideoFrame.timestamp: Duration
    get() = timestampUs.toLong().microseconds

/** Presentation timestamp. */
val VideoDecodedFrame.timestamp: Duration
    get() = timestampUs().toLong().microseconds
