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

private val BILLION = BigInteger.valueOf(1_000_000_000L)

/**
 * This timestamp as a [Duration], rounded down to a whole nanosecond.
 *
 * The [Timestamp] itself is exact; keep it, not the duration, to republish a tick such as 90 kHz.
 * Throws [IllegalArgumentException] when [unscaled], since a group key has no time, when the
 * scale is zero, or past [Duration]'s range, about 146 million years.
 */
fun Timestamp.toDuration(): Duration {
    val scale = requireNotNull(scale) { "unscaled timestamp #$value has no duration" }
    require(scale != 0uL) { "timestamp $value has a zero scale" }
    // Both are u64, past what a Long holds, and the remainder times a billion can exceed 64 bits.
    val units = value.toString().toBigInteger()
    val divisor = scale.toString().toBigInteger()
    val seconds = units / divisor
    require(seconds <= BigInteger.valueOf(Long.MAX_VALUE)) {
        "timestamp $value/$scale s does not fit in a Duration"
    }
    val nanos = units % divisor * BILLION / divisor
    val duration = seconds.toLong().seconds + nanos.toLong().nanoseconds
    // Duration saturates to infinite instead of failing.
    require(duration.isFinite()) { "timestamp $value/$scale s does not fit in a Duration" }
    return duration
}

/**
 * This duration as an exact [Timestamp] at nanosecond scale.
 *
 * Throws [IllegalArgumentException] when negative, infinite, or past 64 bits of nanoseconds
 * (about 584 years); a write refuses one past the value bound with `MoqException.TimeOverflow`.
 */
fun Duration.toTimestamp(): Timestamp {
    require(!isNegative()) { "a timestamp cannot be negative: $this" }
    // `inWholeNanoseconds` saturates instead of failing, so build the count from exact parts.
    require(isFinite()) { "an infinite duration has no timestamp" }
    val nanos = toComponents { seconds, nanoseconds ->
        BigInteger.valueOf(seconds) * BILLION + BigInteger.valueOf(nanoseconds.toLong())
    }
    require(nanos.bitLength() <= 64) { "$this does not fit in 64 bits of nanoseconds" }
    return Timestamp(value = nanos.toString().toULong(), scale = 1_000_000_000uL)
}
