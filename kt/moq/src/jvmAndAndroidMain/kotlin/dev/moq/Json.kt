package dev.moq

import kotlinx.serialization.json.Json
import kotlinx.serialization.serializer
import uniffi.moq.MoqBroadcastProducer

/**
 * The [Json] format used by the typed helpers here and in `dev.moq.json`.
 *
 * Lenient about unknown keys so a producer adding a field does not break older
 * subscribers, matching how the catalog treats untyped sections.
 */
val MoqJson: Json = Json { ignoreUnknownKeys = true }

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
