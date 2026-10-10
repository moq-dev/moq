package dev.moq.json

// JSON tracks, mirroring the moq-json crate. Each type wraps a track: a
// producer takes over a `dev.moq.TrackProducer` and advertises it in the
// broadcast's catalog, and a consumer takes over a `dev.moq.TrackConsumer`
// that has not read a group yet:
//
//     val status = SnapshotProducer(broadcast, broadcast.publishTrack("status", null), SnapshotConfig())
//     val reader = SnapshotConsumer(consumer.subscribeTrack("status", null), SnapshotConfig())

import dev.moq.MoqJson
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.onCompletion
import kotlinx.serialization.serializer

/** Publishes lossy latest-value JSON snapshots on a track it takes over. */
typealias SnapshotProducer = uniffi.moq.MoqJsonSnapshotProducer
/** Consumes reconstructed latest-value JSON snapshots from a track it takes over. */
typealias SnapshotConsumer = uniffi.moq.MoqJsonSnapshotConsumer
/** Publishes a lossless stream of JSON records on a track it takes over. */
typealias StreamProducer = uniffi.moq.MoqJsonStreamProducer
/** Consumes a lossless stream of JSON records from a track it takes over. */
typealias StreamConsumer = uniffi.moq.MoqJsonStreamConsumer
/** Configures a lossy latest-value JSON track. */
typealias SnapshotConfig = uniffi.moq.MoqJsonSnapshotConfig
/** Configures a lossless JSON stream track. */
typealias StreamConfig = uniffi.moq.MoqJsonStreamConfig

/**
 * Publish [value] as a JSON snapshot, superseding the last. A no-op when unchanged.
 *
 * The `@Serializable` type is encoded with [MoqJson]. Pass an already-encoded
 * `String` to serialize with another library: the member overload takes it
 * unchanged.
 */
inline fun <reified T> SnapshotProducer.update(value: T) {
    update(MoqJson.encodeToString(serializer<T>(), value))
}

/** Append [value] to a JSON stream track as one record, encoded with [MoqJson]. */
inline fun <reified T> StreamProducer.append(value: T) {
    append(MoqJson.encodeToString(serializer<T>(), value))
}

/**
 * Stream of JSON values (as strings) from a snapshot track, yielding the latest reconstructed
 * value. A consumer that has fallen behind collapses the backlog to the latest.
 */
fun SnapshotConsumer.values(): Flow<String> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/** Stream of JSON records (as strings) from a stream track, in order. */
fun StreamConsumer.values(): Flow<String> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/**
 * Stream of decoded snapshot values, yielding the latest reconstructed value.
 *
 * A consumer that has fallen behind collapses the backlog to the latest. Use
 * [SnapshotConsumer.values] for the undecoded JSON strings.
 */
inline fun <reified T> SnapshotConsumer.valuesAs(): Flow<T> =
    values().map { MoqJson.decodeFromString(serializer<T>(), it) }

/**
 * Stream of decoded stream records, in order.
 *
 * Use [StreamConsumer.values] for the undecoded JSON strings.
 */
inline fun <reified T> StreamConsumer.valuesAs(): Flow<T> =
    values().map { MoqJson.decodeFromString(serializer<T>(), it) }
