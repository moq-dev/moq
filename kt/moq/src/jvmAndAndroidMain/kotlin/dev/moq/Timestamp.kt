package dev.moq

import java.math.BigInteger
import kotlin.time.Duration
import kotlin.time.Duration.Companion.nanoseconds
import kotlin.time.Duration.Companion.seconds

/**
 * The scale-less timestamp at group key [key] (a datagram's sequence).
 *
 * A scale-less track has no time, so each frame carries its group's key instead; any other key is
 * refused at the write with a `TimestampMismatch`.
 */
fun uniffi.moq.MoqTimestamp.Companion.unscaled(key: ULong): Timestamp =
    Timestamp(value = key, scale = null)

/**
 * This timestamp as a [Duration], rounded down to a whole nanosecond.
 *
 * The [Timestamp] itself is exact; keep it, not the duration, to republish a tick such as 90 kHz.
 * Throws [IllegalArgumentException] when [unscaled], since a group key has no time, or past
 * [Duration]'s range, about 146 million years.
 */
fun Timestamp.toDuration(): Duration {
    val scale = requireNotNull(scale) { "unscaled timestamp #$value has no duration" }
    // The sub-second remainder times a billion can exceed 64 bits.
    val remainder = (value % scale).toString().toBigInteger()
    val divisor = scale.toString().toBigInteger()
    val nanos = remainder * BigInteger.valueOf(1_000_000_000L) / divisor
    val duration = (value / scale).toLong().seconds + nanos.toLong().nanoseconds
    // Duration saturates to infinite instead of failing.
    require(duration.isFinite()) { "timestamp $value/$scale s does not fit in a Duration" }
    return duration
}

/**
 * This duration as an exact [Timestamp] at nanosecond scale.
 *
 * Throws [IllegalArgumentException] when negative; a write refuses one past the value bound with
 * `MoqException.TimeOverflow`.
 */
fun Duration.toTimestamp(): Timestamp {
    require(!isNegative()) { "a timestamp cannot be negative: $this" }
    return Timestamp(value = inWholeNanoseconds.toULong(), scale = 1_000_000_000uL)
}
