package dev.moq

import kotlin.time.Duration
import kotlin.time.Duration.Companion.microseconds

// The FFI surface carries media timestamps as microsecond integers; durations
// already cross as java.time.Duration. Read the timestamps back as a Duration.
// These are extension properties on the typealiased records, so the `*Us`
// fields stay available for anyone building a record to send back across the
// boundary.

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
