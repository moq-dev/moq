package dev.moq

// Re-export the UniFFI types under `dev.moq` so consumers import `dev.moq.*`
// only, never `uniffi.moq.*`. The generated bindings prefix everything with
// `Moq`; dropping that prefix is the kind of per-language convention a generic
// moq-ffi can't apply itself. These are typealiases, not wrappers: the values
// are the exact same objects, so every FFI method and the extensions in
// Flows.kt / Errors.kt apply unchanged.

// Session + connection handles. `Server` is not aliased: `dev.moq.Server` is the
// listen facade (see Server.kt), which exposes the raw handle as `server`.
/** A MoQ client: configure the TLS/bind knobs, then connect to a relay. */
typealias Client = uniffi.moq.MoqClient
/** A live pub/sub session with a relay, exposing a publisher and a consumer. */
typealias Session = uniffi.moq.MoqSession
/** An incoming session awaiting a decision: accept it to handshake, or reject it. */
typealias Request = uniffi.moq.MoqRequest

// Origin (broadcast discovery / announcement).
/** The publish side of an origin: create broadcasts so subscribers can discover them. */
typealias OriginProducer = uniffi.moq.MoqOriginProducer
/** Options for creating an origin, such as its total cache budget. */
typealias OriginOptions = uniffi.moq.MoqOriginOptions
/** The subscribe side of an origin: discover and request announced broadcasts. */
typealias OriginConsumer = uniffi.moq.MoqOriginConsumer
/** A stream of broadcasts requested by subscribers, for serving unannounced paths on demand. */
typealias OriginDynamic = uniffi.moq.MoqOriginDynamic
/** A requested broadcast not yet accepted: fulfill it with a producer or abort it. */
typealias BroadcastRequest = uniffi.moq.MoqBroadcastRequest
/** A stream of broadcast announcements under a prefix. */
typealias Announced = uniffi.moq.MoqAnnounced
/** A pending wait for a specific broadcast path to be announced. */
typealias AnnouncedBroadcast = uniffi.moq.MoqAnnouncedBroadcast
/** A single broadcast announcement: its path plus a consumer. */
typealias Announcement = uniffi.moq.MoqAnnouncement

// Broadcast / track / group producers and consumers.
/** The write side of a broadcast: publish tracks into it. */
typealias BroadcastProducer = uniffi.moq.MoqBroadcastProducer
/** The read side of a broadcast: subscribe to its catalog and tracks. */
typealias BroadcastConsumer = uniffi.moq.MoqBroadcastConsumer
/** Watches a broadcast's route: yields the current route first, then every change. */
typealias RouteWatch = uniffi.moq.MoqRouteWatch
/** Receives tracks requested from a dynamically served broadcast. */
typealias BroadcastDynamic = uniffi.moq.MoqBroadcastDynamic
/** The write side of a raw track: append groups of frames. */
typealias TrackProducer = uniffi.moq.MoqTrackProducer
/** A subscriber-requested track not yet accepted: accept it for a [TrackProducer] or abort it. */
typealias TrackRequest = uniffi.moq.MoqTrackRequest
/** A stream of uncached group requests for one track, for serving fetches on demand. */
typealias TrackDynamic = uniffi.moq.MoqTrackDynamic
/** The read side of a raw track: yields groups in sequence order, skipping ahead if it falls behind. */
typealias TrackConsumer = uniffi.moq.MoqTrackConsumer
/** A request to produce one uncached group for a fetch consumer. */
typealias GroupRequest = uniffi.moq.MoqGroupRequest
/** The write side of a single group: append frames to it. */
typealias GroupProducer = uniffi.moq.MoqGroupProducer
/** The read side of a single group: yields timestamped raw frames. */
typealias GroupConsumer = uniffi.moq.MoqGroupConsumer

// Media (codec-aware) producers and consumers.
/** The write side of a media track fed pre-framed payloads. */
typealias MediaProducer = uniffi.moq.MoqMediaProducer
/** The write side of a media track fed a raw byte stream, with frame boundaries inferred. */
typealias MediaStreamProducer = uniffi.moq.MoqMediaStreamProducer
/** The read side of a media track: yields frames with codec metadata in decode order. */
typealias MediaConsumer = uniffi.moq.MoqMediaConsumer
/** The write side of a raw-audio track; PCM written here is encoded inside the FFI boundary. */
typealias AudioProducer = uniffi.moq.MoqAudioProducer
/** The read side of a raw-audio track: yields decoded PCM frames. */
typealias AudioConsumer = uniffi.moq.MoqAudioConsumer
/** The read side of a broadcast's catalog: yields updates as the set of tracks changes. */
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
/** A broadcast's catalog: its tracks and their properties, plus any application sections. */
typealias Catalog = uniffi.moq.MoqCatalog
/** A datagram-delivered frame, tagged with a per-track sequence number. */
typealias Datagram = uniffi.moq.MoqDatagram
/** A payload plus the timestamp it should be presented at. */
typealias Frame = uniffi.moq.MoqFrame
/** A [Frame] plus the codec metadata a media track carries. */
typealias MediaFrame = uniffi.moq.MoqMediaFrame
/** The catalog description of a video track: codec, dimensions, bitrate, and container. */
typealias Video = uniffi.moq.MoqVideo
/** Caller-provided catalog fields for a video track. */
typealias VideoHint = uniffi.moq.MoqVideoHint
/** Media format, initialization bytes, and optional video hints. */
typealias Init = uniffi.moq.MoqInit
/** The catalog description of an audio track: codec, sample rate, channels, and container. */
typealias Audio = uniffi.moq.MoqAudio
/** A width and height pair, in pixels. */
typealias Dimensions = uniffi.moq.MoqDimensions
/** The route a broadcast takes to reach this origin: relay hop ids (oldest first), advertised cost (lower wins), and whether it's announced. */
typealias Route = uniffi.moq.MoqRoute
/** Tunes how a track subscription is delivered: priority, group ordering, and range. */
typealias Subscription = uniffi.moq.MoqSubscription
/** Options for fetching one past group by sequence. */
typealias FetchGroupOptions = uniffi.moq.MoqFetchGroupOptions
/** Delivery settings for a raw track: priority, ordering, latency budget, and timescale. */
typealias TrackInfo = uniffi.moq.MoqTrackInfo
/** One audio frame: PCM payload bytes plus a presentation timestamp. */
typealias AudioFrame = uniffi.moq.MoqAudioFrame
/** An audio codec identifier. */
typealias AudioCodec = uniffi.moq.MoqAudioCodec
/** A raw PCM sample format, mirroring WebCodecs `AudioData.format`. */
typealias AudioFormat = uniffi.moq.MoqAudioFormat
/** The PCM layout an [AudioConsumer] should decode to. */
typealias AudioDecoderOutput = uniffi.moq.MoqAudioDecoderOutput
/** The PCM layout the caller feeds an [AudioProducer]. */
typealias AudioEncoderInput = uniffi.moq.MoqAudioEncoderInput
/** The codec-side encoder configuration: codec, output rate/channels, bitrate, and frame duration. */
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
