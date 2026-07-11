import MoqFFI

// Plain data types (records + enums) are re-exported under de-prefixed names.
// These carry no behavior, so a typealias keeps them in lockstep with the
// `moq-ffi` crate automatically. The stateful handle types are fully wrapped
// instead (see Client.swift, Broadcast.swift, etc.), so MoqFFI's `Moq`-prefixed
// classes never appear in the public API.

public typealias Frame = MoqFFI.MoqFrame
public typealias Catalog = MoqFFI.MoqCatalog
public typealias Video = MoqFFI.MoqVideo
public typealias Audio = MoqFFI.MoqAudio
public typealias AudioFrame = MoqFFI.MoqAudioFrame
public typealias Dimensions = MoqFFI.MoqDimensions
public typealias AudioEncoderInput = MoqFFI.MoqAudioEncoderInput
public typealias AudioEncoderOutput = MoqFFI.MoqAudioEncoderOutput
public typealias AudioDecoderOutput = MoqFFI.MoqAudioDecoderOutput
public typealias AudioFormat = MoqFFI.MoqAudioFormat
public typealias AudioCodec = MoqFFI.MoqAudioCodec
public typealias Container = MoqFFI.Container
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
