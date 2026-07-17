package dev.moq

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import kotlinx.serialization.json.Json
import kotlinx.serialization.serializer
import uniffi.moq.MoqBroadcastProducer
import uniffi.moq.MoqJsonSnapshotConsumer
import uniffi.moq.MoqJsonSnapshotProducer
import uniffi.moq.MoqJsonStreamConsumer
import uniffi.moq.MoqJsonStreamProducer

/**
 * The [Json] format used by the typed helpers below.
 *
 * Lenient about unknown keys so a producer adding a field does not break older
 * subscribers, matching how the catalog treats untyped sections.
 */
val MoqJson: Json = Json { ignoreUnknownKeys = true }

/**
 * Publish [value] as a JSON snapshot, superseding the last. A no-op when unchanged.
 *
 * The `@Serializable` type is encoded with [MoqJson]. Pass an already-encoded
 * `String` to serialize with another library: the member overload takes it
 * unchanged.
 */
inline fun <reified T> MoqJsonSnapshotProducer.update(value: T) {
    update(MoqJson.encodeToString(serializer<T>(), value))
}

/** Append [value] to a JSON stream track as one record, encoded with [MoqJson]. */
inline fun <reified T> MoqJsonStreamProducer.append(value: T) {
    append(MoqJson.encodeToString(serializer<T>(), value))
}

/**
 * Stream of decoded snapshot values, yielding the latest reconstructed value.
 *
 * A consumer that has fallen behind collapses the backlog to the latest. Use
 * [MoqJsonSnapshotConsumer.values] for the undecoded JSON strings.
 */
inline fun <reified T> MoqJsonSnapshotConsumer.valuesAs(): Flow<T> =
    values().map { MoqJson.decodeFromString(serializer<T>(), it) }

/**
 * Stream of decoded stream records, in order.
 *
 * Use [MoqJsonStreamConsumer.values] for the undecoded JSON strings.
 */
inline fun <reified T> MoqJsonStreamConsumer.valuesAs(): Flow<T> =
    values().map { MoqJson.decodeFromString(serializer<T>(), it) }

/**
 * Set or replace an untyped application section in the catalog.
 *
 * [value] is encoded with [MoqJson] and lands as a top-level catalog key
 * alongside `video`/`audio`, reaching subscribers via `Catalog.sections`. [name]
 * must not be a reserved media section ("video"/"audio"). The catalog is
 * republished automatically. Pass an already-encoded `String` to serialize with
 * another library.
 */
inline fun <reified T> MoqBroadcastProducer.setCatalogSection(name: String, value: T) {
    setCatalogSection(name, MoqJson.encodeToString(serializer<T>(), value))
}
