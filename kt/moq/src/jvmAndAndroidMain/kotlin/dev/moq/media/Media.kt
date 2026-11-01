package dev.moq.media

import dev.moq.pull
import kotlinx.coroutines.flow.Flow
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
/** The read side of a media track: yields frames with codec metadata in decode order. */
typealias TrackConsumer = uniffi.moq.MoqMediaTrackConsumer
/** A finite fetched media group: yields container-decoded frames until the group ends. */
typealias GroupConsumer = uniffi.moq.MoqMediaGroupConsumer
/** The read side of a broadcast's catalog: yields updates as the set of tracks changes. */
typealias CatalogConsumer = uniffi.moq.MoqMediaCatalogConsumer
/** A broadcast's catalog: its tracks and their properties, plus any application sections. */
typealias Catalog = uniffi.moq.MoqCatalog
/** A media [Frame] whose keyframe flag marks group starts or video keyframes; audio flags only group starts. */
typealias MediaFrame = uniffi.moq.MoqMediaFrame
/** One encoded frame, a gap, or an encoder end. */
typealias Read = uniffi.moq.MoqMediaRead
/** An encoded frame and its container metadata. */
typealias ReadFrame = uniffi.moq.MoqMediaRead.Frame
/** Lost media; what follows does not continue it. */
typealias ReadGap = uniffi.moq.MoqMediaRead.Gap
/** Where a stopped encoder's media stops. */
typealias ReadEnd = uniffi.moq.MoqMediaRead.End
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
/** A shared source timestamp offset that does not keep its broadcast open. */
typealias Timebase = uniffi.moq.MoqMediaTimebase
/** A container format, its leading bytes, and an optional shared source timebase. */
typealias ContainerInit = uniffi.moq.MoqContainerInit
/** The catalog description of an audio track: codec, sample rate, channels, whether it is enabled, and container. */
typealias Audio = uniffi.moq.MoqAudio
/** A width and height pair, in pixels. */
typealias Dimensions = uniffi.moq.MoqDimensions
/** Where an audio or video decoder starts on a track that already holds groups: `LATEST` for a live player. */
typealias DecodeStart = uniffi.moq.MoqDecodeStart

/** A weak catalog handle that closes with its broadcast. */
typealias CatalogProducer = uniffi.moq.MoqMediaCatalogProducer
/** A named or requested track target for an importer. */
typealias Target = uniffi.moq.MoqMediaTarget
/** A new track with a chosen or format-derived name. */
typealias Named = uniffi.moq.MoqMediaTarget.Named
/** A subscriber-requested track. */
typealias Requested = uniffi.moq.MoqMediaTarget.Requested
/** A catalog rendition: its ID (catalog key) and its video or audio config. */
typealias Rendition = uniffi.moq.MoqRendition
/** A catalog video rendition. */
typealias VideoRendition = uniffi.moq.MoqRendition.Video
/** A catalog audio rendition. */
typealias AudioRendition = uniffi.moq.MoqRendition.Audio
/** A catalog rendition and live delivery options. */
typealias TrackConfig = uniffi.moq.MoqMediaTrackConfig
/** A catalog rendition's group sequence and fetch delivery options. */
typealias GroupConfig = uniffi.moq.MoqMediaGroupConfig
/** The packaging advertised for a media track. */
typealias Container = uniffi.moq.MoqContainer

/**
 * Stream of catalog updates. Terminates when the underlying track ends.
 *
 * The Flow forwards Kotlin coroutine cancellation to the
 * native consumer's `cancel()` so structured concurrency propagates through
 * to the QUIC stream.
 */
fun CatalogConsumer.updates(): Flow<Catalog> = pull({ cancel() }) { next() }

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

/** Stream of decoded frames from one finite media group. */
fun GroupConsumer.frames(): Flow<MediaFrame> = pull({ cancel() }) { next() }

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

/** The furthest presentation point already read. */
val ReadGap.timestamp: Duration
    get() = timestampUs.toLong().microseconds

/** Where the stopped encoder's media stops. */
val ReadEnd.end: Duration
    get() = endUs.toLong().microseconds
