import MoqFFI

// Plain data types (records + enums) and small objects (e.g. `AudioCodec`)
// are re-exported under de-prefixed names.
// These carry no behavior, so a typealias keeps them in lockstep with the
// `moq-ffi` crate automatically. The stateful handle types are fully wrapped
// instead (see Client.swift, Broadcast.swift, etc.), so MoqFFI's `Moq`-prefixed
// classes never appear in the public API.

/// An exact media timestamp: `value` units at `scale` units per second, or on a scale-less track
/// `Timestamp.unscaled(key)`, whose `value` is the group key (a datagram's sequence) and `scale` nil.
public typealias Timestamp = MoqFFI.MoqTimestamp
/// A payload plus the presentation timestamp it should play at. The unit of
/// every raw write and raw read.
public typealias Frame = MoqFFI.MoqFrame
/// One raw-audio frame: PCM samples in the configured layout plus a
/// presentation timestamp.
public typealias AudioFrame = MoqFFI.MoqAudioFrame
/// The PCM layout (format, sample rate, channels) written to an `Audio.Producer`.
public typealias AudioEncoderInput = MoqFFI.MoqAudioEncoderInput
/// The codec-side audio encoder settings: codec, rate, channels,
/// bitrate, and frame duration.
public typealias AudioEncoderOutput = MoqFFI.MoqAudioEncoderOutput
/// What a `VideoConsumer` decodes to: an optional resize, a delay, and
/// whether frames keep the decoder's native surface.
public typealias VideoDecoderOutput = MoqVideoDecoderOutput
/// One decoded video frame, owning the decoder's surface until released;
/// `pixels(format:)` converts it to packed CPU pixels.
public typealias VideoDecodedFrame = Video.DecodedFrame
/// The decoded PCM layout, its delay (playout and group staleness), and whether the
/// playout delay is estimated.
public typealias AudioDecoderOutput = MoqFFI.MoqAudioDecoderOutput
/// A raw PCM sample format, mirroring WebCodecs `AudioData.format`.
public typealias AudioSampleFormat = MoqFFI.MoqAudioSampleFormat
/// Selects the audio encoder codec. Build one with `AudioCodec.opus()` or `AudioCodec.aac()`.
public typealias AudioCodec = MoqFFI.MoqAudioCodec
/// One raw video frame: pixels in the configured layout plus a presentation
/// timestamp.
public typealias VideoFrame = MoqFFI.MoqVideoFrame
/// The pixel layout, resolution, and framerate written to a `Video.Producer`.
public typealias VideoEncoderInput = MoqFFI.MoqVideoEncoderInput
/// The codec-side video encoder settings: codec, bitrate,
/// keyframe interval, and backend preference.
public typealias VideoEncoderOutput = MoqFFI.MoqVideoEncoderOutput
/// A CPU pixel layout (I420 or RGBA): fed to a `Video.Producer`, or delivered
/// by a `Video.Consumer`.
public typealias VideoPixelFormat = MoqFFI.MoqVideoPixelFormat
/// A video codec identifier (H.264 or H.265).
public typealias VideoCodec = MoqFFI.MoqVideoCodec
/// Which encoder implementation to use: automatic, hardware, software, or one
/// named backend.
public typealias VideoEncoderKind = MoqFFI.MoqVideoEncoderKind
/// A datagram payload with its own sequence and timestamp.
public typealias Datagram = MoqFFI.MoqDatagram
/// Names one continuous stream at a path: `Instance.parse(text:)` names one, only a session mints
/// one, and `==` also compares `isMinted()`, so pin a reported stream with the instance, not its text.
public typealias Instance = MoqFFI.MoqInstance
/// The read-only facts on a received route: the `Instance` of the stream it serves,
/// relay hop ids (oldest first), its static production and link cost, lower wins, and its
/// informational `rtt` to the origin node.
public typealias RouteInfo = MoqFFI.MoqRouteInfo
/// A route over a prefix: the origin-relative `prefix`, what each filter
/// wildcard matched (`captures`, a required list, empty without wildcards), and the `route` serving it.
/// Resolve a path with `OriginConsumer.request`.
public typealias Announce = MoqFFI.MoqAnnounce
/// What an `AnnounceConsumer` yields, each carrying an `Announce`: `.present` once a route
/// covers the path, `.unavailable` and `.available` as a present path loses and recovers a
/// usable path to its origin, `.absent` once the origin withdraws it, and `.offline` once this
/// side lets go of it, which its origin may still serve. Another publisher instance taking a
/// path over keeps it present: hold a `Following` to see the broadcast change.
public typealias AnnounceEvent = MoqFFI.MoqAnnounceEvent
/// Per-subscription delivery preferences: priority, delay, the ranges of the track to
/// deliver, and group ordering.
public typealias Subscription = MoqFFI.MoqSubscription
/// How far behind the live edge a subscriber or decoder plays: `min` is the target latency and
/// where a cold join starts, `nil` to leave it unstated (a decoder plays it as zero), and `max`,
/// `nil` for no subscriber limit, is how far a non-latest group may fall behind before it is
/// skipped. A `min` past `max` starts at `max`.
public typealias Delay = MoqFFI.MoqDelay
/// Options for fetching one complete group by sequence.
public typealias FetchGroupOptions = MoqFFI.MoqFetchGroupOptions
/// Publisher-side track properties: priority, group ordering, max age,
/// and timescale.
public typealias TrackInfo = MoqFFI.MoqTrackInfo
/// How a track's frames are compressed on the wire: `.deflate`, or `nil` in `TrackInfo` for none.
public typealias Compression = MoqFFI.MoqCompression

/// A snapshot of connection statistics (RTT, bandwidth estimates, byte/packet
/// counters). Fields are `nil` when the transport backend doesn't report them.
public typealias ConnectionStats = MoqFFI.MoqConnectionStats

/// Retry pacing for the automatic reconnect; see `Client(backoff:)`. A `nil`
/// field keeps its default. It applies only once a session has been established:
/// a first dial that reaches no session ends the connection with its error rather
/// than retrying.
public typealias Backoff = MoqFFI.MoqBackoff

/// Certificate trust and the mTLS identity for a `Client`.
public typealias ClientTls = MoqFFI.MoqClientTls

/// The served TLS identity for a `Server`: PEM files or generated hostnames.
public typealias ServerTls = MoqFFI.MoqServerTls

/// QUIC transport tuning, such as the peer's inbound stream cap.
public typealias QuicConfig = MoqFFI.MoqQuicConfig

/// The congestion control family for a QUIC connection.
public typealias CongestionControl = MoqFFI.MoqCongestionControl

/// The WebSocket fallback raced against QUIC: whether it runs and QUIC's head start.
public typealias WebSocketConfig = MoqFFI.MoqWebSocketConfig

/// Where a connection is in its lifecycle, carrying that phase's failure or version.
public typealias ConnectionPhase = MoqFFI.MoqConnectionPhase

/// One coherent connection lifecycle snapshot.
public typealias ConnectionState = MoqFFI.MoqConnectionState

/// The network transport carrying an incoming session.
public typealias Transport = MoqFFI.MoqTransport

/// The error thrown by every throwing call in this package. Already conforms to
/// `Swift.Error` and `LocalizedError`; see `Errors.swift` for conveniences.
public typealias MoqError = MoqFFI.MoqError

/// Whether a protocol code is from the session or stream registry.
public typealias ErrorScope = MoqFFI.MoqErrorScope

/// A recognized protocol kind, or `app` / `unknown` when the code is not named.
public typealias ProtocolKind = MoqFFI.MoqProtocolKind

/// A protocol failure: scope, verbatim wire code, kind, and a diagnostic message.
public typealias ProtocolError = MoqFFI.MoqProtocolError

/// A location within a track: a group sequence and a frame index in it.
public typealias Position = MoqFFI.MoqPosition

// Nested rather than top-level: a module-level `Range` would shadow `Swift.Range`
// in every file that imports Moq.
extension Subscription {
    /// A span of a track to deliver: from `start` up to `end`, or onward when `end` is `nil`.
    public typealias Range = MoqFFI.MoqRange
    /// Where one end of a `Range` falls: `.oldest` (a start only), `.absolute(group:)`,
    /// `.relative(groups:)` back from the latest group, or `.time(timestamp:)` for the group
    /// containing it.
    public typealias Anchor = MoqFFI.MoqAnchor
    /// Which pending group the publisher sends first: `.ascending` or `.descending`.
    public typealias Order = MoqFFI.MoqOrder

    /// moq-net's defaults, whose zero `delay` starts at the newest group and skips a superseded
    /// one at once. A `nil` subscription means the same; copy this one to change a field. A
    /// `Subscription(...)` must name `delay`.
    public static var `default`: Subscription { moqSubscriptionDefaults() }
}

/// Publisher priority and optional timestamp scale fixed for a datagram track.
public typealias DatagramInfo = MoqFFI.MoqDatagramInfo
