package dev.moq

import kotlin.time.Instant

/**
 * Mint a fresh publisher epoch from the wall clock and secure randomness, ordered
 * newest last. Mint one per publisher run and announce it in [Route.epoch], so
 * viewers see a restart as a new broadcast instead of a stalled one.
 */
fun mintEpoch(): String = uniffi.moq.moqMintEpoch()

/**
 * The wall-clock time [epoch] encodes, to the millisecond. Throws unless it is a
 * lowercase hyphenated UUIDv7.
 */
fun epochTime(epoch: String): Instant =
    Instant.fromEpochMilliseconds(uniffi.moq.moqEpochTimeMs(epoch).toLong())
