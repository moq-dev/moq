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
	// VideoProperties holds catalog properties shared by every video rendition; nil fields clear those properties.
	VideoProperties = ffi.MoqVideoProperties
	// VideoCodec identifies a published video track's codec: H.264 or H.265.
	VideoCodec = ffi.MoqVideoCodec
	// VideoPixelFormat is a raw pixel layout (I420 or RGBA) written to a VideoProducer.
	VideoPixelFormat = ffi.MoqVideoPixelFormat
	// VideoEncoderInput declares the pixel layout, resolution, and framerate of frames written to a video producer.
	VideoEncoderInput = ffi.MoqVideoEncoderInput
	// VideoEncoderOutput configures the video encoder: codec, optional bitrate and keyframe interval, and backend preference.
	VideoEncoderOutput = ffi.MoqVideoEncoderOutput
	// VideoFrame is one raw video frame: pixels in the configured layout plus a presentation timestamp in microseconds.
	VideoFrame = ffi.MoqVideoFrame
	// VideoEncoderKind selects the encoder implementation. Build one with
	// AutoEncoder, HardwareEncoder, SoftwareEncoder, or NamedEncoder.
	VideoEncoderKind = ffi.MoqVideoEncoderKind
	// VideoEncoderKindAuto is the automatic variant of VideoEncoderKind; build one with AutoEncoder.
	VideoEncoderKindAuto = ffi.MoqVideoEncoderKindAuto
	// VideoEncoderKindHardware is the hardware-only variant of VideoEncoderKind; build one with HardwareEncoder.
	VideoEncoderKindHardware = ffi.MoqVideoEncoderKindHardware
	// VideoEncoderKindSoftware is the software-only variant of VideoEncoderKind; build one with SoftwareEncoder.
	VideoEncoderKindSoftware = ffi.MoqVideoEncoderKindSoftware
	// VideoEncoderKindNamed is the specific-backend variant of VideoEncoderKind; build one with NamedEncoder.
	VideoEncoderKindNamed = ffi.MoqVideoEncoderKindNamed

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

// VideoPixelFormat values: the raw pixel layout fed to the in-process encoder.
const (
	// VideoPixelFormatI420 is tightly-packed planar I420: Y, then U, then V.
	VideoPixelFormatI420 = ffi.MoqVideoPixelFormatI420
	// VideoPixelFormatRgba is tightly-packed RGBA, four bytes per pixel.
	VideoPixelFormatRgba = ffi.MoqVideoPixelFormatRgba
)

// VideoCodec values: the codec a published video track is encoded to.
const (
	// VideoCodecH264 publishes H.264 / AVC as an avc3 track.
	VideoCodecH264 = ffi.MoqVideoCodecH264
	// VideoCodecH265 publishes H.265 / HEVC as a hev1 track. Hardware only, so it
	// fails where no hardware encoder is available.
	VideoCodecH265 = ffi.MoqVideoCodecH265
)

// AutoEncoder prefers a platform hardware encoder, falling back to software.
func AutoEncoder() VideoEncoderKind {
	return VideoEncoderKindAuto{}
}

// HardwareEncoder requires a hardware encoder, failing if none is available.
func HardwareEncoder() VideoEncoderKind {
	return VideoEncoderKindHardware{}
}

// SoftwareEncoder requires the software encoder (openh264, H.264 only).
func SoftwareEncoder() VideoEncoderKind {
	return VideoEncoderKindSoftware{}
}

// NamedEncoder selects a specific backend, e.g. "videotoolbox", "nvenc",
// "vaapi", "mediafoundation", or "openh264".
func NamedEncoder(name string) VideoEncoderKind {
	return VideoEncoderKindNamed{Name: name}
}

// LogLevel configures the native tracing log level (e.g. "info", "debug").
func LogLevel(level string) error {
	return ffi.MoqLogLevel(level)
}
