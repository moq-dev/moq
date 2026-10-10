package dev.moq

// Re-export the UniFFI types under `dev.moq` so consumers import `dev.moq.*`
// only, never `uniffi.moq.*`. The generated bindings prefix everything with
// `Moq`; dropping that prefix is the kind of per-language convention a generic
// moq-ffi can't apply itself. These are typealiases, not wrappers: the values
// are the exact same objects, so every FFI method and the extensions in
// Flows.kt / Errors.kt apply unchanged.

// Session + connection handles. `Server` is not aliased: `dev.moq.Server` is the
// listen facade (see Server.kt), which exposes the raw handle as `server`.
/** A MoQ client built from a [ClientConfig]: connect it to a relay. */
typealias Client = uniffi.moq.MoqClient
/** Client configuration: bind address, versions, TLS, QUIC, WebSocket, reconnect pacing, and origins. */
typealias ClientConfig = uniffi.moq.MoqClientConfig
/** Certificate trust and the mTLS identity for a [ClientConfig]. */
typealias ClientTls = uniffi.moq.MoqClientTls
/** Server configuration: bind address, versions, TLS identity, QUIC, and origins. */
typealias ServerConfig = uniffi.moq.MoqServerConfig
/** The served TLS identity for a [ServerConfig]: PEM files or generated hostnames. */
typealias ServerTls = uniffi.moq.MoqServerTls
/** QUIC transport tuning, such as the peer's inbound stream cap. */
typealias QuicConfig = uniffi.moq.MoqQuicConfig
/** The WebSocket fallback raced against QUIC: whether it runs and QUIC's head start. */
typealias WebSocketConfig = uniffi.moq.MoqWebSocketConfig
/** A live pub/sub session with a relay, exposing publish and consume origins. */
typealias Session = uniffi.moq.MoqSession
/** An incoming session awaiting a decision: accept it to handshake, or reject it. */
typealias Request = uniffi.moq.MoqRequest
/** The network transport carrying an incoming session. */
typealias Transport = uniffi.moq.MoqTransport

// Origin (broadcast discovery / announcement).
/** The publish side of an origin: create broadcasts so subscribers can discover them. */
typealias OriginProducer = uniffi.moq.MoqOriginProducer
/** Config for creating an origin, such as its total cache budget. */
typealias OriginConfig = uniffi.moq.MoqOriginConfig
/** The subscribe side of an origin: discover and request published broadcasts. */
typealias OriginConsumer = uniffi.moq.MoqOriginConsumer
/** A served route: advertises a path prefix and yields broadcast requests beneath it. */
typealias OriginDynamic = uniffi.moq.MoqOriginDynamic
/** A requested broadcast not yet accepted: fulfill it with a producer or reject it. */
typealias BroadcastRequest = uniffi.moq.MoqBroadcastRequest
/** A stream of announce events under a prefix. */
typealias AnnounceConsumer = uniffi.moq.MoqAnnounceConsumer
/** A literal prefix, an optional relative pattern, and the hidden-path opt-in for announcement discovery. */
typealias AnnounceConfig = uniffi.moq.MoqAnnounceConfig
/** A pending wait for a route to cover a specific path. */
typealias AnnouncedBroadcast = uniffi.moq.MoqAnnouncedBroadcast
/** A route over a prefix: its origin-relative path, wildcard captures, and route metadata. */
typealias Announce = uniffi.moq.MoqAnnounce
/**
 * What an [AnnounceConsumer] yields: [AnnounceEventStart], [AnnounceEventUpdate],
 * [AnnounceEventEnd], or [AnnounceEventRestart].
 */
typealias AnnounceEvent = uniffi.moq.MoqAnnounceEvent
// Kotlin cannot reach a sealed class's subtypes through its typealias, so each
// variant gets its own.
/** A route now covers the prefix; the stream had none there. */
typealias AnnounceEventStart = uniffi.moq.MoqAnnounceEvent.Start
/** The route covering the prefix changed hops or cost. */
typealias AnnounceEventUpdate = uniffi.moq.MoqAnnounceEvent.Update
/** No route covers the prefix any more; carries its last route. */
typealias AnnounceEventEnd = uniffi.moq.MoqAnnounceEvent.End
/** Another publisher instance now serves the prefix: drop what was resolved under it and request afresh. */
typealias AnnounceEventRestart = uniffi.moq.MoqAnnounceEvent.Restart
// Broadcast / track / group producers and consumers.
/** The write side of a broadcast: publish tracks into it. */
typealias BroadcastProducer = uniffi.moq.MoqBroadcastProducer
/** The read side of a broadcast: subscribe to its catalog and tracks. */
typealias BroadcastConsumer = uniffi.moq.MoqBroadcastConsumer
/** Receives tracks requested from a dynamically served broadcast. */
typealias BroadcastDynamic = uniffi.moq.MoqBroadcastDynamic
/** The write side of a raw track: append groups of frames. */
typealias TrackProducer = uniffi.moq.MoqTrackProducer
/** A subscriber-requested track not yet accepted: accept it for a [TrackProducer] or abort it. */
typealias TrackRequest = uniffi.moq.MoqTrackRequest
/** A stream of uncached group requests for one track, for serving fetches on demand. */
typealias TrackDynamic = uniffi.moq.MoqTrackDynamic
/** A watch-only handle to whether a published track has subscribers; holding it keeps nothing open. */
typealias TrackDemand = uniffi.moq.MoqTrackDemand
/** The read side of a raw track: yields groups in sequence order, skipping ahead if it falls behind. */
typealias TrackConsumer = uniffi.moq.MoqTrackConsumer
/** A request to produce one uncached group for a fetch consumer. */
typealias GroupRequest = uniffi.moq.MoqGroupRequest
/** The write side of a single group: append frames to it. */
typealias GroupProducer = uniffi.moq.MoqGroupProducer
/** The read side of a single group: yields timestamped raw frames. */
typealias GroupConsumer = uniffi.moq.MoqGroupConsumer

// Media (codec-aware) producers and consumers.
/** A demand-observable raw-audio track producer with explicit timeline re-anchoring after idle gaps. */
typealias AudioProducer = uniffi.moq.MoqAudioProducer
/** The read side of a raw-audio track: yields decoded PCM frames. */
typealias AudioConsumer = uniffi.moq.MoqAudioConsumer
/** The read side of a video track decoded inside the bindings: yields packed frames in the layout asked for. */
typealias VideoConsumer = uniffi.moq.MoqVideoConsumer
/** The write side of a raw-video track; pixels written here are encoded inside the FFI boundary. */
typealias VideoProducer = uniffi.moq.MoqVideoProducer

// Data types.
/** A datagram-delivered frame, tagged with a per-track sequence number. */
typealias Datagram = uniffi.moq.MoqDatagram
/** A payload plus the timestamp it should be presented at. */
typealias Frame = uniffi.moq.MoqFrame
/** A path-prefix route: the publisher epoch it serves (null when unknown), relay hop ids (oldest first), and static production and link cost (lower wins). */
typealias Route = uniffi.moq.MoqRoute
/** Tunes how a track subscription is delivered: priority, group ordering, and range. */
typealias Subscription = uniffi.moq.MoqSubscription
/** Options for fetching one past group by sequence. */
typealias FetchGroupOptions = uniffi.moq.MoqFetchGroupOptions
/** Delivery settings for a raw track: priority, ordering, latency budget, and timescale. */
typealias TrackInfo = uniffi.moq.MoqTrackInfo
/** One audio frame: PCM payload bytes plus a presentation timestamp. */
typealias AudioFrame = uniffi.moq.MoqAudioFrame
/** Selects the audio encoder codec. Build one with `AudioCodec.opus()` or `AudioCodec.aac()`. */
typealias AudioCodec = uniffi.moq.MoqAudioCodec
/** A raw PCM sample format, mirroring WebCodecs `AudioData.format`. */
typealias AudioSampleFormat = uniffi.moq.MoqAudioSampleFormat
/** The PCM layout an [AudioConsumer] should decode to. */
typealias AudioDecoderOutput = uniffi.moq.MoqAudioDecoderOutput
/** What a [VideoConsumer] decodes to: an optional resize, a latency budget, and whether frames keep the decoder's surface (macOS only; refused elsewhere). */
typealias VideoDecoderOutput = uniffi.moq.MoqVideoDecoderOutput
/** One decoded video frame, owning the decoder's surface until closed; `pixels(format)` converts it to packed CPU pixels. */
typealias VideoDecodedFrame = uniffi.moq.MoqVideoDecodedFrame
/** The PCM layout the caller feeds an [AudioProducer]. */
typealias AudioEncoderInput = uniffi.moq.MoqAudioEncoderInput
/** The codec-side encoder configuration: codec, output rate/channels, bitrate, and frame duration. */
typealias AudioEncoderOutput = uniffi.moq.MoqAudioEncoderOutput
/** One video frame: pixels in the configured layout plus a presentation timestamp. */
typealias VideoFrame = uniffi.moq.MoqVideoFrame
/** A video codec identifier (H.264 or H.265). */
typealias VideoCodec = uniffi.moq.MoqVideoCodec
/** A CPU pixel layout (I420 or RGBA): fed to a [VideoProducer], or read from a [VideoDecodedFrame]. */
typealias VideoPixelFormat = uniffi.moq.MoqVideoPixelFormat
/** The pixel layout, resolution, and framerate the caller feeds a [VideoProducer]. */
typealias VideoEncoderInput = uniffi.moq.MoqVideoEncoderInput
/** The video track name, codec, bitrate, keyframe interval, and backend preference. */
typealias VideoEncoderOutput = uniffi.moq.MoqVideoEncoderOutput
/** Which encoder implementation to use: automatic, hardware, software, or one named backend. */
typealias VideoEncoderKind = uniffi.moq.MoqVideoEncoderKind
/** Divides one connection's send estimate among the tracks sharing it. */
typealias Bandwidth = uniffi.moq.MoqBandwidth
/** One track's standing claim on a [Bandwidth]. */
typealias Reservation = uniffi.moq.MoqReservation
/** A snapshot of transport connection statistics. */
typealias ConnectionStats = uniffi.moq.MoqConnectionStats
/** A connection lifecycle transition reported by [Session.status]. */
typealias ConnectionStatus = uniffi.moq.MoqConnectionStatus
/** Retry pacing for the automatic reconnect: initial delay, multiplier, ceiling, and give-up window. */
typealias Backoff = uniffi.moq.MoqBackoff
/** Whether a protocol code is from the session or stream registry. */
typealias ErrorScope = uniffi.moq.MoqErrorScope
/** A recognized protocol kind, or APP / UNKNOWN when the code is not named. */
typealias ProtocolKind = uniffi.moq.MoqProtocolKind
/** A protocol failure: scope, verbatim wire code, kind, and a diagnostic message. */
typealias ProtocolError = uniffi.moq.MoqProtocolError

// NOTE: Kotlin 2.0.21 can't resolve a sealed type's subtypes through a typealias.
// `MoqException` is intentionally NOT aliased, so reference `uniffi.moq.MoqException.Closed`
// directly. `dev.moq.media.Container` aliases `MoqContainer` for signatures, but its
// variants still need the full name (`uniffi.moq.MoqContainer.Loc`). Enums
// (AudioFormat) are fine: entry access through the alias works. Objects
// (AudioCodec) expose constructors through the alias (`AudioCodec.opus()`).
