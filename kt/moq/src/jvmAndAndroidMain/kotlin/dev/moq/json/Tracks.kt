package dev.moq.json

// JSON tracks, mirroring the moq-json crate. Each type wraps a track: a
// producer takes over a `dev.moq.TrackProducer` (the snapshot and stream
// producers also advertise it in the broadcast's catalog, which has no window
// mode), and a consumer takes over a `dev.moq.TrackConsumer` that has not read
// a group yet. Compression is the track's, set through `TrackInfo(compress = true)`:
//
//     val track = broadcast.publishTrack("status", TrackInfo(compress = true))
//     val status = SnapshotProducer(broadcast, track, SnapshotConfig())
//     val reader = SnapshotConsumer(consumer.subscribeTrack("status", null))

import dev.moq.MoqJson
import dev.moq.pull
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import kotlinx.serialization.serializer
import kotlin.time.Duration
import kotlin.time.Duration.Companion.microseconds

/** Publishes lossy latest-value JSON snapshots on a track it takes over. */
typealias SnapshotProducer = uniffi.moq.MoqJsonSnapshotProducer
/** Consumes reconstructed latest-value JSON snapshots from a track it takes over. */
typealias SnapshotConsumer = uniffi.moq.MoqJsonSnapshotConsumer
/** Publishes a lossless stream of JSON records on a track it takes over. */
typealias StreamProducer = uniffi.moq.MoqJsonStreamProducer
/** Consumes a lossless stream of JSON records from a track it takes over. */
typealias StreamConsumer = uniffi.moq.MoqJsonStreamConsumer
/** Configures a lossy latest-value JSON producer. */
typealias SnapshotConfig = uniffi.moq.MoqJsonSnapshotConfig
/** A JSON string and the timestamp of the frame that carried it. */
typealias Value = uniffi.moq.MoqJsonValue
/** Publishes a sliding window of JSON records, pushed at the back and popped from the front. */
typealias WindowProducer = uniffi.moq.MoqJsonWindowProducer
/** Consumes each push, pop, and skip of a JSON window from a track it takes over. */
typealias WindowConsumer = uniffi.moq.MoqJsonWindowConsumer
/** Configures a JSON window producer. */
typealias WindowConfig = uniffi.moq.MoqJsonWindowConfig
/** One change to a window: [WindowEventPush], [WindowEventPop], or [WindowEventSkip]. */
typealias WindowEvent = uniffi.moq.MoqJsonWindowEvent
// Kotlin cannot reach a sealed class's subtypes through its typealias, so each
// variant gets its own.
/** A record joined the window at its absolute index. */
typealias WindowEventPush = uniffi.moq.MoqJsonWindowEvent.Push
/** The records in `start until end` left the window. */
typealias WindowEventPop = uniffi.moq.MoqJsonWindowEvent.Pop
/** The records in `start until end` will never be delivered to this reader. */
typealias WindowEventSkip = uniffi.moq.MoqJsonWindowEvent.Skip
/** A [WindowEvent] and the timestamp of the frame that carried it. */
typealias WindowTimed = uniffi.moq.MoqJsonWindowTimed

/**
 * The frame's timestamp on the broadcast clock, or null when untimed. Rounded down to a whole
 * microsecond, so a tick on a track like 90 kHz may not survive a read and republish exactly.
 */
val Value.timestamp: Duration?
    get() = timestampUs?.toLong()?.microseconds

/**
 * The frame's timestamp on the broadcast clock, or null when untimed. A record first learned of
 * from a group's restatement carries that frame's time, not its original push time.
 */
val WindowTimed.timestamp: Duration?
    get() = timestampUs?.toLong()?.microseconds

/** A decoded JSON value and its frame timestamp on the broadcast clock, or null when untimed. */
data class Timed<T>(val value: T, val timestamp: Duration?)

/** Decode [Value.value] as [T], keeping the timestamp. */
inline fun <reified T> Value.decode(): Timed<T> =
    Timed(MoqJson.decodeFromString(serializer<T>(), value), timestamp)

/**
 * Publish [value] as a JSON snapshot, superseding the last. A no-op when unchanged.
 *
 * The `@Serializable` type is encoded with [MoqJson]. Pass an already-encoded
 * `String` to serialize with another library: the member overload takes it
 * unchanged. [timestampUs] is on the broadcast clock; null is untimed.
 */
inline fun <reified T> SnapshotProducer.update(value: T, timestampUs: ULong? = null) {
    update(MoqJson.encodeToString(serializer<T>(), value), timestampUs)
}

/** Append [value] at [timestampUs] on the broadcast clock; null is untimed. */
inline fun <reified T> StreamProducer.append(value: T, timestampUs: ULong? = null) {
    append(MoqJson.encodeToString(serializer<T>(), value), timestampUs)
}

/** Push [value] onto the window at [timestampUs] on the broadcast clock; null is untimed. */
inline fun <reified T> WindowProducer.push(value: T, timestampUs: ULong? = null) {
    push(MoqJson.encodeToString(serializer<T>(), value), timestampUs)
}

/** Decode the pushed record as [T]. */
inline fun <reified T> WindowEventPush.decode(): T = MoqJson.decodeFromString(serializer<T>(), value)

/**
 * Stream of each JSON state from a snapshot track, in order.
 *
 * Groups older than the subscription's `maxDelay` are skipped, so a playhead reader sets that
 * budget to its distance behind live. Call [SnapshotConsumer.latest] to skip to the newest state.
 */
fun SnapshotConsumer.values(): Flow<Value> = pull({ cancel() }) { next() }

/** Stream of JSON records from a stream track, in order. */
fun StreamConsumer.values(): Flow<Value> = pull({ cancel() }) { next() }

/** Stream of changes to a window, in order. Group rolls never surface. */
fun WindowConsumer.events(): Flow<WindowTimed> = pull({ cancel() }) { next() }

/**
 * Stream of decoded snapshot states, in order.
 *
 * Use [SnapshotConsumer.values] for the undecoded JSON strings.
 */
inline fun <reified T> SnapshotConsumer.valuesAs(): Flow<Timed<T>> =
    values().map { it.decode<T>() }

/**
 * Stream of decoded stream records, in order.
 *
 * Use [StreamConsumer.values] for the undecoded JSON strings.
 */
inline fun <reified T> StreamConsumer.valuesAs(): Flow<Timed<T>> =
    values().map { it.decode<T>() }
