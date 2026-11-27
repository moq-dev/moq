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

/**
 * Client configuration: bind address, versions, tokens, TLS, QUIC, WebSocket, reconnect pacing, and
 * origins.
 */
typealias ClientConfig = uniffi.moq.MoqClientConfig

/** The credentials a [Client] presents, replaceable while it runs. */
typealias Tokens = uniffi.moq.MoqTokens

/**
 * A request token a session presents, which a [Route] or [Claim] names with `setAuth`. Suspend on
 * `answered()` for the peer's verdict; `cancel()` withdraws it unless a route or claim still names
 * it.
 */
typealias Credential = uniffi.moq.MoqCredential

/** Certificate trust and the mTLS identity for a [ClientConfig]. */
typealias ClientTls = uniffi.moq.MoqClientTls

/** Server configuration: bind address, versions, TLS identity, QUIC, and origins. */
typealias ServerConfig = uniffi.moq.MoqServerConfig

/** The served TLS identity for a [ServerConfig]: PEM files or generated hostnames. */
typealias ServerTls = uniffi.moq.MoqServerTls

/** QUIC transport tuning, such as the peer's inbound stream cap. */
typealias QuicConfig = uniffi.moq.MoqQuicConfig

/**
 * The congestion control algorithm for a QUIC connection. Build one with `CongestionControl.bbr()`,
 * `cubic()`, or `unlimited()`; a new algorithm joins as a new constructor named after it.
 */
typealias CongestionControl = uniffi.moq.MoqCongestionControl

/** The WebSocket fallback raced against QUIC: whether it runs and QUIC's head start. */
typealias WebSocketConfig = uniffi.moq.MoqWebSocketConfig

/** A live pub/sub session with a relay, exposing publish and consume origins. */
typealias Session = uniffi.moq.MoqSession

/** An incoming session awaiting a decision: accept it to handshake, or reject it. */
typealias Request = uniffi.moq.MoqRequest

/** The network transport carrying an incoming session. */
typealias Transport = uniffi.moq.MoqTransport

// Origin (broadcast discovery / announcement).
/** The publish side of an origin: create routes and claims so subscribers can discover them. */
typealias OriginProducer = uniffi.moq.MoqOriginProducer

/**
 * An exact route on an origin, owning one broadcast (`broadcast()` to fill it). `announce`,
 * `withdraw`, `setCost`, and `setAuth` advertise it. Closing or cancelling it retracts it, as does
 * ending its broadcast; a broadcast handle alone does not keep it announced.
 */
typealias Route = uniffi.moq.MoqRoute

/**
 * A prefix route on an origin, from `OriginProducer.claim`: a claim that every path beneath a
 * prefix can be served, yielding each request (`requested()`). `announce`, `withdraw`, `setCost`,
 * and `setAuth` advertise it. Closing or cancelling it retracts it and rejects the requests still
 * waiting.
 */
typealias Claim = uniffi.moq.MoqClaim

/** Config for creating an origin, such as its total cache budget. */
typealias OriginConfig = uniffi.moq.MoqOriginConfig

/** The subscribe side of an origin: discover and request published broadcasts. */
typealias OriginConsumer = uniffi.moq.MoqOriginConsumer

/** A requested broadcast not yet answered: serve it, route it, or reject it. */
typealias BroadcastRequest = uniffi.moq.MoqBroadcastRequest

/** A stream of announce events under a prefix. */
typealias AnnounceConsumer = uniffi.moq.MoqAnnounceConsumer

/**
 * A literal prefix, an optional relative pattern, and the hidden-path opt-in for announcement
 * discovery.
 */
typealias AnnounceConfig = uniffi.moq.MoqAnnounceConfig

/** Follows a specific broadcast path across the publisher identities that serve it. */
typealias Following = uniffi.moq.MoqFollowing

/**
 * A route over a prefix: its origin-relative path, required wildcard captures (empty without
 * wildcards), and route metadata.
 */
typealias Announce = uniffi.moq.MoqAnnounce

/**
 * What an [AnnounceConsumer] yields: [AnnounceEventPresent] or [AnnounceEventAbsent].
 *
 * Another publisher identity taking a path over keeps it present: hold a [Following] to see the
 * broadcast change.
 */
typealias AnnounceEvent = uniffi.moq.MoqAnnounceEvent

// Kotlin cannot reach a sealed class's subtypes through its typealias, so each
// variant gets its own.
/** A route now covers the path, which was absent; it may arrive unavailable. */
typealias AnnounceEventPresent = uniffi.moq.MoqAnnounceEvent.Present

/**
 * No route covers the path any more, whether its origin withdrew it or this side let go of it;
 * carries its last route.
 */
typealias AnnounceEventAbsent = uniffi.moq.MoqAnnounceEvent.Absent

// Broadcast / track / group producers and consumers.
/** The write side of a broadcast: publish tracks into it. */
typealias BroadcastProducer = uniffi.moq.MoqBroadcastProducer

/** The read side of a broadcast: subscribe to its catalog and tracks. */
typealias BroadcastConsumer = uniffi.moq.MoqBroadcastConsumer

/** Receives tracks requested from a dynamically served broadcast. */
typealias BroadcastRequests = uniffi.moq.MoqBroadcastRequests

/** The write side of a raw track: append groups of frames. */
typealias TrackProducer = uniffi.moq.MoqTrackProducer

/** A subscriber-requested track not yet accepted: accept it for a [TrackProducer] or reject it. */
typealias TrackRequest = uniffi.moq.MoqTrackRequest

/** A stream of the fills readers of one track need: runs of groups the cache lacks. */
typealias TrackRequests = uniffi.moq.MoqTrackRequests

/**
 * A watch-only handle to whether a published track has subscribers; holding it keeps nothing open.
 */
typealias TrackDemand = uniffi.moq.MoqTrackDemand

/**
 * The read side of a raw track: yields groups in start order, skipping ahead if it falls behind.
 */
typealias TrackConsumer = uniffi.moq.MoqTrackConsumer

/** A run of groups readers miss: accept each group you have and drop the rest. */
typealias Fill = uniffi.moq.MoqFill

/** The write side of a single group: append frames to it. */
typealias GroupProducer = uniffi.moq.MoqGroupProducer

/** The read side of a single group: yields timestamped raw frames. */
typealias GroupConsumer = uniffi.moq.MoqGroupConsumer

// Data types.
/**
 * A datagram payload with its own sequence and timestamp, the [unscaled] sequence on a scale-less
 * track.
 */
typealias Datagram = uniffi.moq.MoqDatagram

/**
 * A payload plus the timestamp it should be presented at, or its group's [unscaled] key on a
 * scale-less track.
 */
typealias Frame = uniffi.moq.MoqFrame

/**
 * An exact timestamp: `value` units at `scale` units per second, or on a scale-less track a null
 * `scale` and the group key (a datagram's sequence) as `value`; see [unscaled].
 */
typealias Timestamp = uniffi.moq.MoqTimestamp

/**
 * Names one continuous stream at a path: `Identity.parse` names one, only a session mints one, and
 * equality also compares `isMinted()`, so pin a reported stream with the identity, not its text.
 */
typealias Identity = uniffi.moq.MoqIdentity

/**
 * How a [Route] advertises itself: `hidden` until `announce`, its static production cost (lower
 * wins), an optional stable [Identity] (null mints a fresh one the route keeps for its lifetime),
 * and the epoch, which only matters with a stable identity.
 */
typealias RouteOptions = uniffi.moq.MoqRouteOptions

/**
 * The read-only facts on a received route: the [Identity] of the stream it serves, static
 * production and link cost (lower wins), and whether a usable path reaches it.
 */
typealias RouteInfo = uniffi.moq.MoqRouteInfo

/** Tunes how a track subscription is delivered: priority, delay, ranges, and group ordering. */
typealias Subscription = uniffi.moq.MoqSubscription

/**
 * How far behind the live edge a subscriber or decoder plays: `min` is the target latency, and once
 * stated, zero included, a [Range] with no start is a live join that far behind the live edge; null
 * leaves it unstated, so such a range reaches back as far as `max` allows (a decoder starts it as
 * zero). `max`, null for no subscriber limit, is how far a non-latest group may fall behind before
 * it is skipped, and how far newer media may run past a missing group before a decoder gives it up.
 * A `min` past `max` starts a live join no further back than the group covering the point `max`
 * behind the live edge.
 */
typealias Delay = uniffi.moq.MoqDelay

/**
 * moq-net's defaults, whose `delay` (no `min`, a zero `max`) starts at the newest group and skips a
 * superseded one at once. A null subscription means the same; `copy` this one to change a field. A
 * `Subscription(...)` must name `delay`.
 */
fun uniffi.moq.MoqSubscription.Companion.default(): Subscription =
    uniffi.moq.moqSubscriptionDefaults()

/**
 * A span of a track to deliver: from the group containing `start` through the group containing
 * `end`, each an [unscaled] time in the track's timescale (a scaled one is refused). Every end
 * includes its whole group. A null `start` is no start, which the subscription's [Delay] places: a
 * stated `min` is a live join that far behind the live edge (zero is the latest group), and an
 * unstated one reaches back as far as `max` allows. A null `end` keeps following the live edge.
 */
typealias Range = uniffi.moq.MoqRange

/**
 * Which group comes first, as the publisher sends them and `recvGroup` returns those held:
 * `ASCENDING` or `DESCENDING`. Before moq-lite-07 the publisher sends newest first whatever this
 * asks.
 */
typealias Order = uniffi.moq.MoqOrder

/** A location within a track: a group start and a frame index in it. */
typealias Position = uniffi.moq.MoqPosition

/** Options for fetching one past group by start. */
typealias FetchGroupOptions = uniffi.moq.MoqFetchGroupOptions

/** Delivery settings for a raw track: priority, max age, refresh, timescale, and compression. */
typealias TrackInfo = uniffi.moq.MoqTrackInfo

/**
 * When a subscriber asks a track's publisher for a new group, which [TrackDemand.refresh] reports:
 * a join once the newest group outweighs `ratio` times its first frame (any join when `null`),
 * waiting out `interval` (`null` for no wait), which caps how often the publisher makes groups
 * however many subscribers ask. A bare `TrackRefresh()` asks for nothing.
 */
typealias TrackRefresh = uniffi.moq.MoqTrackRefresh

/**
 * How a track's frames are compressed on the wire: `Compression.deflate()`, or `null` in
 * [TrackInfo] for none. Compare a received one with `==` or by `name()`.
 */
typealias Compression = uniffi.moq.MoqCompression

/** One audio frame: PCM payload bytes plus a presentation timestamp. */
typealias AudioFrame = uniffi.moq.MoqAudioFrame

/** Selects the audio encoder codec. Build one with `AudioCodec.opus()` or `AudioCodec.aac()`. */
typealias AudioCodec = uniffi.moq.MoqAudioCodec

/** A raw PCM sample format, mirroring WebCodecs `AudioData.format`. */
typealias AudioSampleFormat = uniffi.moq.MoqAudioSampleFormat

/** The PCM layout an audio decoder should decode to. */
typealias AudioDecoderOutput = uniffi.moq.MoqAudioDecoderOutput

/**
 * What a video decoder decodes to: an optional resize, a latency budget, and whether frames keep
 * the decoder's surface (macOS only; refused elsewhere).
 */
typealias VideoDecoderOutput = uniffi.moq.MoqVideoDecoderOutput

/**
 * One decoded video frame, owning the decoder's surface until closed; `pixels(format)` converts it
 * to packed CPU pixels.
 */
typealias VideoDecodedFrame = dev.moq.video.DecodedFrame

/** The PCM layout the caller feeds an audio producer. */
typealias AudioEncoderInput = uniffi.moq.MoqAudioEncoderInput

/**
 * The codec-side encoder configuration: codec, output rate/channels, bitrate, and frame duration.
 */
typealias AudioEncoderOutput = uniffi.moq.MoqAudioEncoderOutput

/** One video frame: pixels in the configured layout plus a presentation timestamp. */
typealias VideoFrame = uniffi.moq.MoqVideoFrame

/** Selects the video encoder codec. Build one with `VideoCodec.h264()` or `VideoCodec.h265()`. */
typealias VideoCodec = uniffi.moq.MoqVideoCodec

/**
 * A CPU pixel layout (I420 or RGBA): fed to a video producer, or read from a [VideoDecodedFrame].
 */
typealias VideoPixelFormat = uniffi.moq.MoqVideoPixelFormat

/** The pixel layout, resolution, and framerate the caller feeds a video producer. */
typealias VideoEncoderInput = uniffi.moq.MoqVideoEncoderInput

/** The video codec, bitrate, keyframe interval, and backend preference. */
typealias VideoEncoderOutput = uniffi.moq.MoqVideoEncoderOutput

/** Which encoder implementation to use: automatic, hardware, software, or one named backend. */
typealias VideoEncoderKind = uniffi.moq.MoqVideoEncoderKind

/** Divides one connection's send estimate among the tracks sharing it. */
typealias Allocator = uniffi.moq.MoqAllocator

/** An owned bandwidth estimate; release it to stop receive PROBE demand. */
typealias BandwidthConsumer = uniffi.moq.MoqBandwidthConsumer

/** One track's standing claim on an [Allocator]. */
typealias Reservation = uniffi.moq.MoqReservation

/** Divides one connection's receive estimate among the broadcasts watched over it. */
typealias Viewers = uniffi.moq.MoqViewers

/**
 * One watched broadcast's standing claim on [Viewers]; holding it requests PROBE, closing it
 * releases the claim.
 */
typealias Viewer = uniffi.moq.MoqViewer

/** Turns a [Reservation]'s grant into a sender's [RateTarget]; call `update` on every frame. */
typealias RateControl = uniffi.moq.MoqRateControl

/** How a [RateControl] maps a grant onto a bitrate, and the lower rendition that disables it. */
typealias RatePolicy = uniffi.moq.MoqRatePolicy

/** What a sender following a [RateControl] should be doing: an enabled bitrate, or disabled. */
typealias RateTarget = uniffi.moq.MoqRateTarget

/** A snapshot of transport connection statistics. */
typealias ConnectionStats = uniffi.moq.MoqConnectionStats

/** Follows the grant a session's peer gave it; cancel it to release a pending wait. */
typealias GrantConsumer = uniffi.moq.MoqGrantConsumer

/** What the peer lets this side do: publish and subscribe patterns, and an expiry. */
typealias Grant = uniffi.moq.MoqGrant

/** Where a connection is in its lifecycle, carrying that phase's failure or version. */
typealias ConnectionPhase = uniffi.moq.MoqConnectionPhase

// Kotlin cannot reach a sealed class's subtypes through its typealias, so each
// variant gets its own.
/** No session has connected yet; carries the latest failed dial. */
typealias ConnectionPhaseConnecting = uniffi.moq.MoqConnectionPhase.Connecting

/** A session is serving; carries its negotiated version. */
typealias ConnectionPhaseConnected = uniffi.moq.MoqConnectionPhase.Connected

/** No session is serving while the connection redials; carries why. */
typealias ConnectionPhaseDisconnected = uniffi.moq.MoqConnectionPhase.Disconnected

/**
 * The old session serves while its replacement dials; carries its version and the replacement's
 * failure.
 */
typealias ConnectionPhaseMigrating = uniffi.moq.MoqConnectionPhase.Migrating

/** The connection has stopped for good; carries the terminal error, if any. */
typealias ConnectionPhaseClosed = uniffi.moq.MoqConnectionPhase.Closed

/** One coherent connection lifecycle snapshot. */
typealias ConnectionState = uniffi.moq.MoqConnectionState

/**
 * Retry pacing for the automatic reconnect: initial delay, multiplier, ceiling, and give-up window.
 * A null field keeps its default, zero means zero (retry at once, or give up at the first failure),
 * and `maxUnlimited` lifts the ceiling. Until the peer has accepted a session's credential or
 * drained a session with a GOAWAY (or, on moq-lite 03/04, which never answer it, a session stayed
 * up 1s), a failed dial or a closed session, such as a refused credential, ends the connection with
 * its error rather than retrying.
 */
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
// (DecodeStart) are fine: entry access through the alias works. Objects
// (AudioCodec, AudioFormat, VideoPixelFormat, Compression) expose constructors
// through the alias (`AudioFormat.opus()`).

/** Writes live datagrams with an independent sequence counter and no cache. */
typealias DatagramProducer = uniffi.moq.MoqDatagramProducer

/** Reads a live datagram track in arrival order from its own cursor. */
typealias DatagramConsumer = uniffi.moq.MoqDatagramConsumer

/** A requested datagram track awaiting acceptance or rejection. */
typealias DatagramRequest = uniffi.moq.MoqDatagramRequest

/** Serves on-demand datagram tracks through their own request queue. */
typealias DatagramRequests = uniffi.moq.MoqDatagramRequests

/** Publisher priority and optional timestamp scale fixed for a datagram track. */
typealias DatagramInfo = uniffi.moq.MoqDatagramInfo
