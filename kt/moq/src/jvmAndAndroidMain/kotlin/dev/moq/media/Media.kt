package dev.moq.media

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.onCompletion
import kotlin.time.Duration
import kotlin.time.Duration.Companion.microseconds
import dev.moq.Frame
import dev.moq.MoqJson
import kotlinx.serialization.serializer
import uniffi.moq.MoqException

/** The write side of a media track; discontinuity() marks a break between pre-framed payloads. */
typealias TrackProducer = uniffi.moq.MoqMediaTrackProducer
/** The write side of a media track fed a raw byte stream, with frame boundaries inferred. */
typealias TrackStreamProducer = uniffi.moq.MoqMediaTrackStreamProducer
/** The write side of a container, which publishes each track it describes. */
typealias ContainerProducer = uniffi.moq.MoqMediaContainerProducer
/** The write side of a container fed a raw byte stream. */
typealias ContainerStreamProducer = uniffi.moq.MoqMediaContainerStreamProducer
/** The read side of a media track: yields frames with codec metadata in decode order. */
typealias ContainerConsumer = uniffi.moq.MoqMediaContainerConsumer
/** A finite fetched media group: yields container-decoded frames until the group ends. */
typealias ContainerGroupConsumer = uniffi.moq.MoqMediaContainerGroupConsumer
/** The read side of a broadcast's catalog: yields updates as the set of tracks changes. */
typealias CatalogConsumer = uniffi.moq.MoqMediaCatalogConsumer
/** A broadcast's catalog: its tracks and their properties, plus any application sections. */
typealias Catalog = uniffi.moq.MoqCatalog
/** A media [Frame] whose keyframe flag marks group starts or video keyframes; audio flags only group starts. */
typealias MediaFrame = uniffi.moq.MoqMediaFrame
/** The catalog description of a video track, including whether it is enabled (a disabled one has no frames coming). */
typealias Video = uniffi.moq.MoqVideo
/** Caller-provided catalog fields for a video track. */
typealias VideoHint = uniffi.moq.MoqVideoHint
/** A single audio codec an importer can parse. */
typealias AudioFormat = uniffi.moq.MoqAudioFormat
/** A single video codec an importer can parse. */
typealias VideoFormat = uniffi.moq.MoqVideoFormat
/** A container that publishes its own tracks. */
typealias ContainerFormat = uniffi.moq.MoqContainerFormat
/** Catalog properties shared by every video rendition; absent fields clear those properties. */
typealias VideoProperties = uniffi.moq.MoqVideoProperties
/** An audio codec, its required init bytes, and an optional label. */
typealias AudioInit = uniffi.moq.MoqAudioInit
/** A video codec, optional init bytes, a label, and catalog hints. */
typealias VideoInit = uniffi.moq.MoqVideoInit
/** A container format and its leading bytes. */
typealias ContainerInit = uniffi.moq.MoqContainerInit
/** The catalog description of an audio track: codec, sample rate, channels, whether it is enabled, and container. */
typealias Audio = uniffi.moq.MoqAudio
/** A width and height pair, in pixels. */
typealias Dimensions = uniffi.moq.MoqDimensions

/** A weak catalog handle that closes with its broadcast. */
typealias CatalogProducer = uniffi.moq.MoqMediaCatalogProducer
/** A named or requested track target for an importer. */
typealias Target = uniffi.moq.MoqMediaTarget
/** A new track with a chosen or format-derived name. */
typealias Named = uniffi.moq.MoqMediaTarget.Named
/** A subscriber-requested track. */
typealias Requested = uniffi.moq.MoqMediaTarget.Requested
/** A track name, container, and live delivery options. */
typealias ContainerConfig = uniffi.moq.MoqMediaContainerConfig
/** A fetched group name, sequence, container, and delivery options. */
typealias ContainerGroupConfig = uniffi.moq.MoqMediaContainerGroupConfig
/** The packaging advertised for a media track. */
typealias Container = uniffi.moq.MoqContainer

/**
 * Stream of catalog updates. Terminates when the underlying track ends.
 *
 * The Flow's [onCompletion] forwards Kotlin coroutine cancellation to the
 * native consumer's `cancel()` so structured concurrency propagates through
 * to the QUIC stream.
 */
fun CatalogConsumer.updates(): Flow<Catalog> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/**
 * Subscribe to the catalog track and return the first catalog, cancelling the
 * subscription before returning. Convenience for callers that only need the
 * current catalog rather than a stream of updates (use [updates] for that).
 */
suspend fun catalog(broadcast: dev.moq.BroadcastConsumer): Catalog {
    val consumer = CatalogConsumer.subscribe(broadcast)
    try {
        return consumer.next() ?: throw MoqException.Closed()
    } finally {
        consumer.cancel()
    }
}

/** Stream of decoded media frames in decode order. */
fun ContainerConsumer.frames(): Flow<MediaFrame> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/** Stream of decoded frames from one finite, fetched media group. */
fun ContainerGroupConsumer.frames(): Flow<MediaFrame> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/** Presentation timestamp of a container-decoded frame. */
val MediaFrame.timestamp: Duration
    get() = timestampUs.toLong().microseconds

/**
 * Set an application catalog section from a serializable value, encoded with [MoqJson].
 *
 * A `String` argument resolves to the member `setSection(name, json)` instead, so it must
 * already be encoded JSON.
 */
inline fun <reified T> CatalogProducer.setSection(name: String, value: T) {
    setSection(name, MoqJson.encodeToString(serializer<T>(), value))
}
