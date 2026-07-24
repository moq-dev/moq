import MoqFFI

// Plain data types (records + enums) are re-exported under de-prefixed names.
// These carry no behavior, so a typealias keeps them in lockstep with the
// `moq-ffi` crate automatically. The stateful handle types are fully wrapped
// instead (see Client.swift, Broadcast.swift, etc.), so MoqFFI's `Moq`-prefixed
// classes never appear in the public API.

public typealias Frame = MoqFFI.MoqFrame
/// A frame plus the codec metadata a media track carries.
public typealias MediaFrame = MoqFFI.MoqMediaFrame
public typealias Catalog = MoqFFI.MoqCatalog
public typealias Video = MoqFFI.MoqVideo
/// Caller-provided catalog fields for a video track.
public typealias VideoHint = MoqFFI.MoqVideoHint
/// Video presentation metadata applied to all video renditions in the catalog.
public typealias VideoPresentation = MoqFFI.MoqVideoPresentation
public typealias Audio = MoqFFI.MoqAudio
public typealias AudioFrame = MoqFFI.MoqAudioFrame
public typealias Dimensions = MoqFFI.MoqDimensions
public typealias AudioEncoderInput = MoqFFI.MoqAudioEncoderInput
public typealias AudioEncoderOutput = MoqFFI.MoqAudioEncoderOutput
public typealias AudioDecoderOutput = MoqFFI.MoqAudioDecoderOutput
public typealias AudioFormat = MoqFFI.MoqAudioFormat
public typealias AudioCodec = MoqFFI.MoqAudioCodec
public typealias Container = MoqFFI.MoqContainer
public typealias Datagram = MoqFFI.MoqDatagram
/// The route a broadcast takes to reach this origin: relay hop ids (oldest
/// first) plus the publisher's advertised cost (lower wins).
public typealias Route = MoqFFI.MoqRoute
public typealias Subscription = MoqFFI.MoqSubscription
/// Options for fetching one complete group by sequence.
public typealias FetchGroupOptions = MoqFFI.MoqFetchGroupOptions
public typealias TrackInfo = MoqFFI.MoqTrackInfo

/// A snapshot of connection statistics (RTT, bandwidth estimates, byte/packet
/// counters). Fields are `nil` when the transport backend doesn't report them.
public typealias ConnectionStats = MoqFFI.MoqConnectionStats

/// The error thrown by every throwing call in this package. Already conforms to
/// `Swift.Error` and `LocalizedError`; see `Errors.swift` for conveniences.
public typealias MoqError = MoqFFI.MoqError
