package moq

import ffi "moq.dev/moq-ffi/moq"

// Record and enum types re-exported from the ffi layer without the Moq prefix.
// These are plain data, so type aliases are exact: a moq.AudioFrame is an
// ffi.MoqAudioFrame, constructible and comparable across the boundary.
type (
	// Audio describes one audio rendition in a broadcast catalog: codec, sample rate, channel count, and container.
	Audio = ffi.MoqAudio
	// AudioCodec identifies an audio track's codec; Opus is currently the only value.
	AudioCodec = ffi.MoqAudioCodec
	// AudioDecoderOutput configures the PCM format, sample rate, and channels DecodeAudio delivers.
	AudioDecoderOutput = ffi.MoqAudioDecoderOutput
	// AudioEncoderInput declares the PCM sample format, sample rate, and channel count of frames written to an audio producer.
	AudioEncoderInput = ffi.MoqAudioEncoderInput
	// AudioEncoderOutput configures the Opus encoder: codec, optional sample rate, channels, bitrate, and frame duration.
	AudioEncoderOutput = ffi.MoqAudioEncoderOutput
	// AudioSampleFormat is a raw PCM sample layout, mirroring WebCodecs AudioData.format.
	AudioSampleFormat = ffi.MoqAudioSampleFormat
	// AudioFrame is one audio frame: PCM payload plus a presentation timestamp in microseconds.
	AudioFrame = ffi.MoqAudioFrame
	// Catalog is a broadcast's manifest: its video and audio renditions plus display metadata.
	Catalog = ffi.MoqCatalog
	// ConnectionStats holds transport metrics for a session (RTT, bandwidth, byte and packet counters); each field is nil when unreported.
	ConnectionStats = ffi.MoqConnectionStats
	// ConnectionStatus is a connection lifecycle transition reported by Session.Status.
	ConnectionStatus = ffi.MoqConnectionStatus
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
	// Subscription holds subscriber-side delivery preferences: priority, ordering, max age, and group range.
	Subscription = ffi.MoqSubscription
	// TrackInfo holds publisher-side track properties: priority, ordering, max age, and timescale.
	TrackInfo = ffi.MoqTrackInfo
	// Video describes one catalog rendition, including whether the publisher recommends temporarily avoiding it.
	Video = ffi.MoqVideo
	// VideoHint supplies catalog fields a video stream can't reveal itself, such as bitrate, filling only the gaps.
	// VideoDecodedFrame is one decoded video frame: packed I420, its dimensions, and a timestamp in microseconds.
	VideoDecodedFrame = ffi.MoqVideoDecodedFrame
	// VideoDecoderOutput configures what DecodeVideo delivers: an optional resize plus a max age.
	VideoDecoderOutput = ffi.MoqVideoDecoderOutput
	VideoHint = ffi.MoqVideoHint
	// AudioFormat is a single audio codec an importer can parse.
	AudioFormat = ffi.MoqAudioFormat
	// VideoFormat is a single video codec an importer can parse.
	VideoFormat = ffi.MoqVideoFormat
	// ContainerFormat is a container that publishes its own tracks.
	ContainerFormat = ffi.MoqContainerFormat
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

// AudioFormat values: a single audio codec an importer can parse.
const (
	// AudioFormatAac is Advanced Audio Coding, configured by an AudioSpecificConfig.
	AudioFormatAac = ffi.MoqAudioFormatAac
	// AudioFormatOpus is Opus, configured by an OpusHead.
	AudioFormatOpus = ffi.MoqAudioFormatOpus
	// AudioFormatFlac is FLAC, configured by its STREAMINFO block.
	AudioFormatFlac = ffi.MoqAudioFormatFlac
	// AudioFormatMp3 is MPEG-1/2 Audio Layer III.
	AudioFormatMp3 = ffi.MoqAudioFormatMp3
)

// VideoFormat values: a single video codec an importer can parse. The two H.26x pairs
// differ by framing, not codec: Avc1/Hvc1 are length-prefixed with an out-of-band config
// record, Avc3/Hev1 are Annex-B with the parameter sets inline.
const (
	// VideoFormatAvc1 is H.264 with length-prefixed NALUs and an out-of-band avcC.
	VideoFormatAvc1 = ffi.MoqVideoFormatAvc1
	// VideoFormatAvc3 is H.264 in Annex-B with inline SPS/PPS.
	VideoFormatAvc3 = ffi.MoqVideoFormatAvc3
	// VideoFormatHvc1 is H.265 with length-prefixed NALUs and an out-of-band hvcC.
	VideoFormatHvc1 = ffi.MoqVideoFormatHvc1
	// VideoFormatHev1 is H.265 in Annex-B with inline parameter sets.
	VideoFormatHev1 = ffi.MoqVideoFormatHev1
	// VideoFormatAv01 is AV1.
	VideoFormatAv01 = ffi.MoqVideoFormatAv01
	// VideoFormatVp8 is VP8.
	VideoFormatVp8 = ffi.MoqVideoFormatVp8
	// VideoFormatVp9 is VP9.
	VideoFormatVp9 = ffi.MoqVideoFormatVp9
)

// ContainerFormat values: a container that publishes its own tracks.
const (
	// ContainerFormatFmp4 is fragmented MP4 / CMAF.
	ContainerFormatFmp4 = ffi.MoqContainerFormatFmp4
	// ContainerFormatMkv is Matroska / WebM.
	ContainerFormatMkv = ffi.MoqContainerFormatMkv
	// ContainerFormatTs is an MPEG-2 transport stream.
	ContainerFormatTs = ffi.MoqContainerFormatTs
	// ContainerFormatFlv is Flash Video, as used by RTMP.
	ContainerFormatFlv = ffi.MoqContainerFormatFlv
)

// AudioSampleFormat values: the raw PCM sample layout fed to or returned from the
// in-process Opus codec.
const (
	// AudioSampleFormatU8 is unsigned 8-bit interleaved PCM.
	AudioSampleFormatU8 = ffi.MoqAudioSampleFormatU8
	// AudioSampleFormatS16 is signed 16-bit interleaved PCM.
	AudioSampleFormatS16 = ffi.MoqAudioSampleFormatS16
	// AudioSampleFormatS32 is signed 32-bit interleaved PCM.
	AudioSampleFormatS32 = ffi.MoqAudioSampleFormatS32
	// AudioSampleFormatF32 is 32-bit float interleaved PCM.
	AudioSampleFormatF32 = ffi.MoqAudioSampleFormatF32
	// AudioSampleFormatU8Planar is unsigned 8-bit planar PCM, one buffer per channel.
	AudioSampleFormatU8Planar = ffi.MoqAudioSampleFormatU8Planar
	// AudioSampleFormatS16Planar is signed 16-bit planar PCM, one buffer per channel.
	AudioSampleFormatS16Planar = ffi.MoqAudioSampleFormatS16Planar
	// AudioSampleFormatS32Planar is signed 32-bit planar PCM, one buffer per channel.
	AudioSampleFormatS32Planar = ffi.MoqAudioSampleFormatS32Planar
	// AudioSampleFormatF32Planar is 32-bit float planar PCM, one buffer per channel.
	AudioSampleFormatF32Planar = ffi.MoqAudioSampleFormatF32Planar
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

// NamedEncoder selects a specific backend these bindings compile:
// "videotoolbox" (macOS), "mediafoundation" (Windows), or "openh264"
// (software, everywhere). Naming one this build lacks fails with a no-encoder
// error.
func NamedEncoder(name string) VideoEncoderKind {
	return VideoEncoderKindNamed{Name: name}
}

// ConnectionStatus values: the lifecycle of a client session's connection.
const (
	// StatusConnected means a session connected (the first connect, or a reconnect after a drop).
	StatusConnected = ffi.MoqConnectionStatusConnected
	// StatusDisconnected means the session dropped; a reconnect attempt follows.
	StatusDisconnected = ffi.MoqConnectionStatusDisconnected
	// StatusMigrating means the peer sent a GOAWAY; the replacement is being dialed while the old session keeps serving.
	StatusMigrating = ffi.MoqConnectionStatusMigrating
)

// LogLevel configures the native tracing log level (e.g. "info", "debug").
func LogLevel(level string) error {
	return ffi.MoqLogLevel(level)
}
