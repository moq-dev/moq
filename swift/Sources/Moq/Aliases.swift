import MoqFFI

// Plain data types (records + enums) are re-exported under de-prefixed names.
// These carry no behavior, so a typealias keeps them in lockstep with the
// `moq-ffi` crate automatically. The stateful handle types are fully wrapped
// instead (see Client.swift, Broadcast.swift, etc.), so MoqFFI's `Moq`-prefixed
// classes never appear in the public API.

/// A payload plus the presentation timestamp it should play at. The unit of
/// every raw write and raw read.
public typealias Frame = MoqFFI.MoqFrame
/// A frame plus the codec metadata a media track carries.
public typealias MediaFrame = MoqFFI.MoqMediaFrame
/// The JSON manifest describing a broadcast's tracks: video and audio
/// renditions, display geometry, and untyped application sections.
public typealias Catalog = MoqFFI.MoqCatalog
/// A video rendition in the catalog: codec, coded/display dimensions, bitrate,
/// framerate, and container.
public typealias Video = MoqFFI.MoqVideo
/// Caller-provided catalog fields for a video track.
public typealias VideoHint = MoqFFI.MoqVideoHint
/// An audio rendition in the catalog: codec, sample rate, channel count,
/// bitrate, and container.
public typealias Audio = MoqFFI.MoqAudio
/// One raw-audio frame: PCM samples in the configured layout plus a
/// presentation timestamp.
public typealias AudioFrame = MoqFFI.MoqAudioFrame
/// A width and height in pixels.
public typealias Dimensions = MoqFFI.MoqDimensions
/// The PCM layout (format, sample rate, channels) written to an `AudioProducer`.
public typealias AudioEncoderInput = MoqFFI.MoqAudioEncoderInput
/// The encoder-side config for a published audio track: codec, rate, channels,
/// bitrate, and frame duration.
public typealias AudioEncoderOutput = MoqFFI.MoqAudioEncoderOutput
/// The PCM layout an `AudioConsumer` decodes to, plus its latency budget.
public typealias AudioDecoderOutput = MoqFFI.MoqAudioDecoderOutput
/// A raw PCM sample format, mirroring WebCodecs `AudioData.format`.
public typealias AudioFormat = MoqFFI.MoqAudioFormat
/// An audio codec identifier (e.g. Opus).
public typealias AudioCodec = MoqFFI.MoqAudioCodec
/// How a track's frames are packaged (Legacy, CMAF, or LOC), as advertised in
/// the catalog.
public typealias Container = MoqFFI.MoqContainer
/// A best-effort raw-track datagram as received: sequence, timestamp, and payload.
public typealias Datagram = MoqFFI.MoqDatagram
/// The route a broadcast takes to reach this origin: relay hop ids (oldest
/// first) plus the publisher's advertised cost (lower wins).
public typealias Route = MoqFFI.MoqRoute
/// Per-subscription delivery preferences: priority, group ordering, latency
/// budget, and group range.
public typealias Subscription = MoqFFI.MoqSubscription
/// Options for fetching one complete group by sequence.
public typealias FetchGroupOptions = MoqFFI.MoqFetchGroupOptions
/// Publisher-side track properties: priority, group ordering, latency budget,
/// and timescale.
public typealias TrackInfo = MoqFFI.MoqTrackInfo

/// A snapshot of connection statistics (RTT, bandwidth estimates, byte/packet
/// counters). Fields are `nil` when the transport backend doesn't report them.
public typealias ConnectionStats = MoqFFI.MoqConnectionStats

/// The error thrown by every throwing call in this package. Already conforms to
/// `Swift.Error` and `LocalizedError`; see `Errors.swift` for conveniences.
public typealias MoqError = MoqFFI.MoqError
