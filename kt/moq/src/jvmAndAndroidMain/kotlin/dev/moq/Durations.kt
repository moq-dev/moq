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

/** Delay before the first reconnect attempt. */
val Backoff.initial: Duration
    get() = initialUs.toLong().microseconds

/** Maximum delay between reconnect attempts. */
val Backoff.max: Duration
    get() = maxUs.toLong().microseconds

/** Time spent retrying before giving up. [Duration.ZERO] retries forever. */
val Backoff.timeout: Duration
    get() = timeoutUs.toLong().microseconds

/** Upper bound on buffering before a stalled group is skipped. */
val Subscription.maxAge: Duration
    get() = maxAgeUs.toLong().microseconds

/** Maximum age of a non-latest group before the publisher evicts it, or null for the default. */
val TrackInfo.maxAge: Duration?
    get() = maxAgeUs?.toLong()?.microseconds

/** Upper bound on buffering before a stalled group is skipped, or null for the default. */
val AudioDecoderOutput.maxAge: Duration?
    get() = maxAgeUs?.toLong()?.microseconds

/** Upper bound on buffering before a stalled group is skipped, or null for the default. */
val VideoDecoderOutput.maxAge: Duration?
    get() = maxAgeUs?.toLong()?.microseconds

/** Encoded frame duration. */
val AudioEncoderOutput.frameDuration: Duration
    get() = frameDurationUs.toLong().microseconds

/** Presentation timestamp. */
val Frame.timestamp: Duration
    get() = timestampUs.toLong().microseconds

/** Presentation timestamp. */
val MediaFrame.timestamp: Duration
    get() = timestampUs.toLong().microseconds

/** Presentation timestamp. */
val Datagram.timestamp: Duration
    get() = timestampUs.toLong().microseconds

/** Presentation timestamp of the first sample. */
val AudioFrame.timestamp: Duration
    get() = timestampUs.toLong().microseconds

/** Presentation timestamp. */
val VideoFrame.timestamp: Duration
    get() = timestampUs.toLong().microseconds

/** Presentation timestamp. */
val VideoDecodedFrame.timestamp: Duration
    get() = timestampUs().toLong().microseconds
