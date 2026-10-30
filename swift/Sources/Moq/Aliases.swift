import MoqFFI

// Plain data types (records + enums) and small objects (e.g. `AudioCodec`)
// are re-exported under de-prefixed names.
// These carry no behavior, so a typealias keeps them in lockstep with the
// `moq-ffi` crate automatically. The stateful handle types are fully wrapped
// instead (see Client.swift, Broadcast.swift, etc.), so MoqFFI's `Moq`-prefixed
// classes never appear in the public API.

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
/// What a `VideoConsumer` decodes to: an optional resize, a max delay, and
/// whether frames keep the decoder's native surface.
public typealias VideoDecoderOutput = MoqVideoDecoderOutput
/// One decoded video frame, owning the decoder's surface until released;
/// `pixels(format:)` converts it to packed CPU pixels.
public typealias VideoDecodedFrame = MoqVideoDecodedFrame
/// The decoded PCM layout, maximum group staleness, and playout delay.
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
/// A best-effort raw-track datagram as received: sequence, timestamp, and payload.
public typealias Datagram = MoqFFI.MoqDatagram
/// How a broadcast advertises its exact path: its static production `cost`,
/// lower wins, and the publisher `epoch` it serves (`nil` when unknown).
public typealias AnnounceRoute = MoqFFI.MoqAnnounceRoute
/// An observed path-prefix route: the publisher `epoch` it serves (`nil` when
/// unknown), relay hop ids (oldest first), and its static production and link
/// cost, lower wins.
public typealias Route = MoqFFI.MoqRoute
/// A route over a prefix: the origin-relative `prefix`, what each filter
/// wildcard matched (`captures`, `nil` for a partial overlap), and the `route`
/// serving it. Resolve a path with `OriginConsumer.request`.
public typealias Announce = MoqFFI.MoqAnnounce
/// What an `AnnounceConsumer` yields: `.start`, `.update`, `.end`, or
/// `.restart` (request the path again), each carrying an `Announce`.
public typealias AnnounceEvent = MoqFFI.MoqAnnounceEvent
/// Per-subscription delivery preferences: priority, latency budget, the ranges
/// of the track to deliver, and group ordering.
public typealias Subscription = MoqFFI.MoqSubscription
/// Options for fetching one complete group by sequence.
public typealias FetchGroupOptions = MoqFFI.MoqFetchGroupOptions
/// Publisher-side track properties: priority, group ordering, max age,
/// and timescale.
public typealias TrackInfo = MoqFFI.MoqTrackInfo

/// A snapshot of connection statistics (RTT, bandwidth estimates, byte/packet
/// counters). Fields are `nil` when the transport backend doesn't report them.
public typealias ConnectionStats = MoqFFI.MoqConnectionStats

/// Retry pacing for the automatic reconnect; see `Client(backoff:)`. A `nil`
/// field keeps its default.
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

/// The connection lifecycle phase.
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

// Nested rather than top-level: a module-level `Range` would shadow `Swift.Range`
// in every file that imports Moq.
extension Subscription {
    /// A span of a track to deliver: from `start` up to `end`, or onward when `end` is `nil`.
    public typealias Range = MoqFFI.MoqRange
    /// Where one end of a `Range` falls: `.absolute(group:)`, `.relative(groups:)`
    /// back from the latest group, or `.time(timestampUs:)` for the group containing it.
    public typealias Anchor = MoqFFI.MoqAnchor
    /// Which pending group the publisher sends first: `.ascending` or `.descending`.
    public typealias Order = MoqFFI.MoqOrder
}
