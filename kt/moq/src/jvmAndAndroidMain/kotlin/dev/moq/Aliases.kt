package dev.moq

// Re-export the UniFFI types under `dev.moq` so consumers import `dev.moq.*`
// only, never `uniffi.moq.*`. The generated bindings prefix everything with
// `Moq`; dropping that prefix is the kind of per-language convention a generic
// moq-ffi can't apply itself. These are typealiases, not wrappers: the values
// are the exact same objects, so every FFI method and the extensions in
// Flows.kt / Errors.kt apply unchanged.

// Session + connection handles.
typealias Client = uniffi.moq.MoqClient
typealias Session = uniffi.moq.MoqSession
typealias Server = uniffi.moq.MoqServer
typealias Request = uniffi.moq.MoqRequest

// Origin (broadcast discovery / announcement).
typealias OriginProducer = uniffi.moq.MoqOriginProducer
typealias OriginConsumer = uniffi.moq.MoqOriginConsumer
typealias Announced = uniffi.moq.MoqAnnounced
typealias AnnouncedBroadcast = uniffi.moq.MoqAnnouncedBroadcast
typealias Announcement = uniffi.moq.MoqAnnouncement

// Broadcast / track / group producers and consumers.
typealias BroadcastProducer = uniffi.moq.MoqBroadcastProducer
typealias BroadcastConsumer = uniffi.moq.MoqBroadcastConsumer
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

// Data types.
typealias Catalog = uniffi.moq.MoqCatalog
typealias Frame = uniffi.moq.MoqFrame
typealias Video = uniffi.moq.MoqVideo
typealias Audio = uniffi.moq.MoqAudio
typealias Dimensions = uniffi.moq.MoqDimensions
typealias Subscription = uniffi.moq.MoqSubscription
typealias FetchGroupOptions = uniffi.moq.MoqFetchGroupOptions
typealias TrackInfo = uniffi.moq.MoqTrackInfo
typealias AudioFrame = uniffi.moq.MoqAudioFrame
typealias AudioCodec = uniffi.moq.MoqAudioCodec
typealias AudioFormat = uniffi.moq.MoqAudioFormat
typealias AudioDecoderOutput = uniffi.moq.MoqAudioDecoderOutput
typealias AudioEncoderInput = uniffi.moq.MoqAudioEncoderInput
typealias AudioEncoderOutput = uniffi.moq.MoqAudioEncoderOutput

// NOTE: a few types are intentionally NOT aliased. `Container` (sealed) and
// `MoqException` (sealed) need subtype access (`Container.Loc`,
// `MoqException.Closed`), which Kotlin 2.0.21 can't resolve through a typealias.
// Reference those as `uniffi.moq.Container` / `uniffi.moq.MoqException`. Enums
// (AudioCodec/AudioFormat) are fine: entry access through the alias works.
