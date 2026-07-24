package dev.moq

// Re-export the UniFFI types under `dev.moq` so consumers import `dev.moq.*`
// only, never `uniffi.moq.*`. The generated bindings prefix everything with
// `Moq`; dropping that prefix is the kind of per-language convention a generic
// moq-ffi can't apply itself. These are typealiases, not wrappers: the values
// are the exact same objects, so every FFI method and the extensions in
// Flows.kt / Errors.kt apply unchanged.

// Session + connection handles. `Server` is not aliased: `dev.moq.Server` is the
// listen facade (see Server.kt), which exposes the raw handle as `server`.
typealias Client = uniffi.moq.MoqClient
typealias Session = uniffi.moq.MoqSession
typealias Request = uniffi.moq.MoqRequest

// Origin (broadcast discovery / announcement).
typealias OriginProducer = uniffi.moq.MoqOriginProducer
typealias OriginOptions = uniffi.moq.MoqOriginOptions
typealias OriginConsumer = uniffi.moq.MoqOriginConsumer
typealias OriginDynamic = uniffi.moq.MoqOriginDynamic
typealias BroadcastRequest = uniffi.moq.MoqBroadcastRequest
typealias Announced = uniffi.moq.MoqAnnounced
typealias AnnouncedBroadcast = uniffi.moq.MoqAnnouncedBroadcast
typealias Announcement = uniffi.moq.MoqAnnouncement

// Broadcast / track / group producers and consumers.
typealias BroadcastProducer = uniffi.moq.MoqBroadcastProducer
typealias BroadcastConsumer = uniffi.moq.MoqBroadcastConsumer
/** Watches a broadcast's route: yields the current route first, then every change. */
typealias RouteWatch = uniffi.moq.MoqRouteWatch
/** Receives tracks requested from a dynamically served broadcast. */
typealias BroadcastDynamic = uniffi.moq.MoqBroadcastDynamic
typealias TrackProducer = uniffi.moq.MoqTrackProducer
typealias TrackRequest = uniffi.moq.MoqTrackRequest
typealias TrackDynamic = uniffi.moq.MoqTrackDynamic
typealias TrackConsumer = uniffi.moq.MoqTrackConsumer
typealias GroupRequest = uniffi.moq.MoqGroupRequest
typealias GroupProducer = uniffi.moq.MoqGroupProducer
typealias GroupConsumer = uniffi.moq.MoqGroupConsumer

// Media (codec-aware) producers and consumers.
typealias MediaProducer = uniffi.moq.MoqMediaProducer
typealias MediaStreamProducer = uniffi.moq.MoqMediaStreamProducer
typealias MediaConsumer = uniffi.moq.MoqMediaConsumer
typealias AudioProducer = uniffi.moq.MoqAudioProducer
typealias AudioConsumer = uniffi.moq.MoqAudioConsumer
typealias CatalogConsumer = uniffi.moq.MoqCatalogConsumer
/** Publishes lossy latest-value JSON snapshots. */
typealias JsonSnapshotProducer = uniffi.moq.MoqJsonSnapshotProducer
/** Consumes reconstructed latest-value JSON snapshots. */
typealias JsonSnapshotConsumer = uniffi.moq.MoqJsonSnapshotConsumer
/** Publishes a lossless stream of JSON records. */
typealias JsonStreamProducer = uniffi.moq.MoqJsonStreamProducer
/** Consumes a lossless stream of JSON records. */
typealias JsonStreamConsumer = uniffi.moq.MoqJsonStreamConsumer

// Data types.
typealias Catalog = uniffi.moq.MoqCatalog
typealias Datagram = uniffi.moq.MoqDatagram
typealias Frame = uniffi.moq.MoqFrame
/** A [Frame] plus the codec metadata a media track carries. */
typealias MediaFrame = uniffi.moq.MoqMediaFrame
typealias Video = uniffi.moq.MoqVideo
/** Caller-provided catalog fields for a video track. */
typealias VideoHint = uniffi.moq.MoqVideoHint
/** Video presentation metadata applied to all video renditions in the catalog. */
typealias VideoPresentation = uniffi.moq.MoqVideoPresentation
/** Media format, initialization bytes, and optional video hints. */
typealias Init = uniffi.moq.MoqInit
typealias Audio = uniffi.moq.MoqAudio
typealias Dimensions = uniffi.moq.MoqDimensions
/** The route a broadcast takes to reach this origin: relay hop ids (oldest first) plus advertised cost (lower wins). */
typealias Route = uniffi.moq.MoqRoute
typealias Subscription = uniffi.moq.MoqSubscription
typealias FetchGroupOptions = uniffi.moq.MoqFetchGroupOptions
typealias TrackInfo = uniffi.moq.MoqTrackInfo
typealias AudioFrame = uniffi.moq.MoqAudioFrame
typealias AudioCodec = uniffi.moq.MoqAudioCodec
typealias AudioFormat = uniffi.moq.MoqAudioFormat
typealias AudioDecoderOutput = uniffi.moq.MoqAudioDecoderOutput
typealias AudioEncoderInput = uniffi.moq.MoqAudioEncoderInput
typealias AudioEncoderOutput = uniffi.moq.MoqAudioEncoderOutput
/** A snapshot of transport connection statistics. */
typealias ConnectionStats = uniffi.moq.MoqConnectionStats
/** Configures a lossy latest-value JSON track. */
typealias JsonSnapshotConfig = uniffi.moq.MoqJsonSnapshotConfig
/** Configures a lossless JSON stream track. */
typealias JsonStreamConfig = uniffi.moq.MoqJsonStreamConfig

// NOTE: a few types are intentionally NOT aliased. `MoqContainer` (sealed) and
// `MoqException` (sealed) need subtype access (`MoqContainer.Loc`,
// `MoqException.Closed`), which Kotlin 2.0.21 can't resolve through a typealias.
// Reference those as `uniffi.moq.MoqContainer` / `uniffi.moq.MoqException`. Enums
// (AudioCodec/AudioFormat) are fine: entry access through the alias works.
