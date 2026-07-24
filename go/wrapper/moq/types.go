package moq

import ffi "github.com/moq-dev/moq-go-ffi/moq"

// Record and enum types re-exported from the ffi layer without the Moq prefix.
// These are plain data, so type aliases are exact: a moq.AudioFrame is an
// ffi.MoqAudioFrame, constructible and comparable across the boundary.
type (
	// Audio describes one audio rendition in a broadcast catalog: codec, sample rate, channel count, and container.
	Audio = ffi.MoqAudio
	// AudioCodec identifies an audio track's codec; Opus is currently the only value.
	AudioCodec = ffi.MoqAudioCodec
	// AudioDecoderOutput configures the PCM format, sample rate, and channels SubscribeAudio delivers.
	AudioDecoderOutput = ffi.MoqAudioDecoderOutput
	// AudioEncoderInput declares the PCM sample format, sample rate, and channel count of frames written to an audio producer.
	AudioEncoderInput = ffi.MoqAudioEncoderInput
	// AudioEncoderOutput configures the Opus encoder: codec, optional sample rate, channels, bitrate, and frame duration.
	AudioEncoderOutput = ffi.MoqAudioEncoderOutput
	// AudioFormat is a raw PCM sample layout, mirroring WebCodecs AudioData.format.
	AudioFormat = ffi.MoqAudioFormat
	// AudioFrame is one audio frame: PCM payload plus a presentation timestamp in microseconds.
	AudioFrame = ffi.MoqAudioFrame
	// Catalog is a broadcast's manifest: its video and audio renditions plus display metadata.
	Catalog = ffi.MoqCatalog
	// ConnectionStats holds transport metrics for a session (RTT, bandwidth, byte and packet counters); each field is nil when unreported.
	ConnectionStats = ffi.MoqConnectionStats
	// Datagram is a best-effort track datagram as received: sequence number, timestamp, and payload.
	Datagram = ffi.MoqDatagram
	// Dimensions is a width and height in pixels.
	Dimensions = ffi.MoqDimensions
	// Frame is a raw track frame: a payload and its presentation timestamp in microseconds.
	Frame = ffi.MoqFrame
	// MediaFrame is a Frame plus the codec-derived keyframe flag carried on a media track.
	MediaFrame = ffi.MoqMediaFrame
	// FetchGroupOptions configures a single FetchGroup call, currently just the delivery priority.
	FetchGroupOptions = ffi.MoqFetchGroupOptions
	// OriginOptions configures a new origin, such as its maximum cache size in bytes.
	OriginOptions = ffi.MoqOriginOptions
	// Route is the hop chain a broadcast takes to reach an origin, its cost, and whether it's announced.
	Route = ffi.MoqRoute
	// Subscription holds subscriber-side delivery preferences: priority, ordering, latency budget, and group range.
	Subscription = ffi.MoqSubscription
	// TrackInfo holds publisher-side track properties: priority, ordering, latency budget, and timescale.
	TrackInfo = ffi.MoqTrackInfo
	// Video describes one video rendition in a broadcast catalog: codec, dimensions, bitrate, framerate, and container.
	Video = ffi.MoqVideo
	// VideoHint supplies catalog fields a video stream can't reveal itself, such as bitrate, filling only the gaps.
	VideoHint = ffi.MoqVideoHint

	// Container selects how subscribed media frames are demuxed. Build one with
	// LegacyContainer, CmafContainer, or LocContainer.
	Container = ffi.MoqContainer
	// ContainerLegacy is the legacy hang container variant of Container; build one with LegacyContainer.
	ContainerLegacy = ffi.MoqContainerLegacy
	// ContainerCmaf is the CMAF (fMP4) container variant of Container, carrying its init segment; build one with CmafContainer.
	ContainerCmaf = ffi.MoqContainerCmaf
	// ContainerLoc is the low-overhead container variant of Container; build one with LocContainer.
	ContainerLoc = ffi.MoqContainerLoc
)

// LegacyContainer selects the legacy hang container for a media subscription.
func LegacyContainer() Container {
	return ContainerLegacy{}
}

// CmafContainer selects the CMAF (fMP4) container for a media subscription,
// initialized from the given init segment.
func CmafContainer(init []byte) Container {
	return ContainerCmaf{Init: init}
}

// LocContainer selects the low-overhead container for a media subscription.
func LocContainer() Container {
	return ContainerLoc{}
}

// AudioFormat values: the raw PCM sample layout fed to or returned from the
// in-process Opus codec.
const (
	// AudioFormatU8 is unsigned 8-bit interleaved PCM.
	AudioFormatU8 = ffi.MoqAudioFormatU8
	// AudioFormatS16 is signed 16-bit interleaved PCM.
	AudioFormatS16 = ffi.MoqAudioFormatS16
	// AudioFormatS32 is signed 32-bit interleaved PCM.
	AudioFormatS32 = ffi.MoqAudioFormatS32
	// AudioFormatF32 is 32-bit float interleaved PCM.
	AudioFormatF32 = ffi.MoqAudioFormatF32
	// AudioFormatU8Planar is unsigned 8-bit planar PCM, one buffer per channel.
	AudioFormatU8Planar = ffi.MoqAudioFormatU8Planar
	// AudioFormatS16Planar is signed 16-bit planar PCM, one buffer per channel.
	AudioFormatS16Planar = ffi.MoqAudioFormatS16Planar
	// AudioFormatS32Planar is signed 32-bit planar PCM, one buffer per channel.
	AudioFormatS32Planar = ffi.MoqAudioFormatS32Planar
	// AudioFormatF32Planar is 32-bit float planar PCM, one buffer per channel.
	AudioFormatF32Planar = ffi.MoqAudioFormatF32Planar
)

// AudioCodecOpus is the only codec currently supported for raw audio tracks.
const AudioCodecOpus = ffi.MoqAudioCodecOpus

// LogLevel configures the native tracing log level (e.g. "info", "debug").
func LogLevel(level string) error {
	return ffi.MoqLogLevel(level)
}
