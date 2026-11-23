package moq

import (
	ffi "moq.dev/moq-ffi/moq"
)

// Record, enum, and small object types re-exported from the ffi layer without the Moq prefix.
// These are plain data, so type aliases are exact: a moq.AudioFrame is an
// ffi.MoqAudioFrame, constructible and comparable across the boundary.
type (
	// AudioCodec selects the audio encoder codec. Build one with OpusAudioCodec or AacAudioCodec;
	// adding a codec later adds a constructor, not a breaking enum change.
	AudioCodec = ffi.MoqAudioCodec
	// AudioEncoderInput declares the PCM sample format, sample rate, and channel count of frames written to an audio producer.
	AudioEncoderInput = ffi.MoqAudioEncoderInput
	// AudioEncoderOutput configures the encoder: codec, optional sample rate, channels, bitrate, and frame duration.
	AudioEncoderOutput = ffi.MoqAudioEncoderOutput
	// AudioSampleFormat is a raw PCM sample layout, mirroring WebCodecs AudioData.format.
	AudioSampleFormat = ffi.MoqAudioSampleFormat
	// AudioFrame is one audio frame: PCM payload plus a presentation Timestamp.
	AudioFrame = ffi.MoqAudioFrame
	// CongestionControl is the congestion control algorithm for a QUIC connection. Build one with
	// BBRCongestionControl, CubicCongestionControl, or UnlimitedCongestionControl; a new algorithm
	// joins as a new builder named after it.
	CongestionControl = ffi.MoqCongestionControl
	// ErrorScope is whether a protocol code is from the session or stream registry.
	ErrorScope = ffi.MoqErrorScope
	// FetchGroupOptions configures a single FetchGroup call, including its optional freshness budget.
	FetchGroupOptions = ffi.MoqFetchGroupOptions
	// ProtocolKind is a recognized protocol kind, or App / Unknown when the code is not named.
	ProtocolKind = ffi.MoqProtocolKind
	// OriginConfig configures a new origin, such as its maximum cache size in bytes.
	OriginConfig = ffi.MoqOriginConfig
	// RouteInfo is the read-only facts on a received route: the Instance of the stream it serves (routes with one instance are one continuous stream), its static production and link cost (lower wins), and whether a usable path reaches it (Available).
	RouteInfo = ffi.MoqRouteInfo
	// Instance names one continuous stream at a path. Compare two with Eq: a session mints one
	// for a route whose wire carries none, and it differs from a named instance with the same Text.
	Instance = ffi.MoqInstance
	// Announce is a route over a prefix: the origin-relative Prefix, what each filter
	// wildcard matched (Captures), and the RouteInfo serving it. It carries no broadcast;
	// resolve a path with [OriginConsumer.Request]. Captures is a required slice,
	// empty when the filter has no wildcards.
	Announce = ffi.MoqAnnounce
	// AnnounceEvent is what an AnnounceConsumer yields: AnnounceEventPresent or AnnounceEventAbsent.
	// Another publisher instance taking a path over keeps it present; see it with [OriginConsumer.Follow].
	AnnounceEvent = ffi.MoqAnnounceEvent
	// AnnounceEventPresent reports a route now covering a path that was absent; it may arrive unavailable.
	AnnounceEventPresent = ffi.MoqAnnounceEventPresent
	// AnnounceEventAbsent reports that no route covers a path any more, whether its origin withdrew it
	// or this side let go of it, carrying its last route.
	AnnounceEventAbsent = ffi.MoqAnnounceEventAbsent
	// Range is a span of a track to deliver: from the group containing Start through the group
	// containing End, each an Unscaled Timestamp in the track's timescale (a scaled one is
	// refused). Every start is the first frame of its group, and every end includes its whole
	// group. A nil Start is no start, which the subscription's Delay places: a stated Min is a
	// live join that far behind the live edge (zero is the latest group), and an unstated one
	// reaches back as far as Max allows. A nil End keeps following the live edge.
	Range = ffi.MoqRange
	// Order is which of a subscription's pending groups the publisher sends first.
	Order = ffi.MoqOrder
	// Compression is how a track's frames are compressed on the wire. Build one with
	// DeflateCompression, and compare a received one by Name; a new compression joins as a
	// new builder.
	Compression = ffi.MoqCompression
	// Position is a location within a track: a group start and a frame index in it.
	Position = ffi.MoqPosition
	// VideoCodec selects the video encoder codec. Build one with H264VideoCodec or
	// H265VideoCodec; adding a codec later adds a constructor, not a breaking enum change.
	VideoCodec = ffi.MoqVideoCodec
	// VideoPixelFormat is a CPU pixel layout (I420 or RGBA): written to video.Producer, or read from a VideoDecodedFrame.
	VideoPixelFormat = ffi.MoqVideoPixelFormat
	// VideoEncoderInput declares the pixel layout, resolution, and framerate of frames written to a video producer.
	VideoEncoderInput = ffi.MoqVideoEncoderInput
	// VideoEncoderOutput configures the video codec, bitrate, keyframe interval, and backend preference.
	VideoEncoderOutput = ffi.MoqVideoEncoderOutput
	// VideoFrame is one raw video frame: pixels in the configured layout plus a presentation Timestamp.
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
)

// OpusAudioCodec selects Opus (RFC 6716) for an audio encoder.
func OpusAudioCodec() *AudioCodec {
	return ffi.MoqAudioCodecOpus()
}

// AacAudioCodec selects AAC-LC through the platform's encoder for an audio encoder.
// A host without one refuses it. Leave FrameDuration at 0 for AAC's own frame.
func AacAudioCodec() *AudioCodec {
	return ffi.MoqAudioCodecAac()
}

// I420VideoPixelFormat is tightly-packed planar I420: Y, then U, then V.
func I420VideoPixelFormat() *VideoPixelFormat {
	return ffi.MoqVideoPixelFormatI420()
}

// RgbaVideoPixelFormat is tightly-packed RGBA, four bytes per pixel.
func RgbaVideoPixelFormat() *VideoPixelFormat {
	return ffi.MoqVideoPixelFormatRgba()
}

// H264VideoCodec publishes H.264 / AVC as an avc3 track.
func H264VideoCodec() *VideoCodec {
	return ffi.MoqVideoCodecH264()
}

// H265VideoCodec publishes H.265 / HEVC as a hev1 track. Hardware only, so it
// fails where no hardware encoder is available.
func H265VideoCodec() *VideoCodec {
	return ffi.MoqVideoCodecH265()
}

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
// "videotoolbox" (macOS, iOS), "mediafoundation" (Windows), or "openh264"
// (software, everywhere). Naming one this build lacks fails with a no-encoder
// error.
func NamedEncoder(name string) VideoEncoderKind {
	return VideoEncoderKindNamed{Name: name}
}

// Order values: which pending group the publisher sends first.
const (
	// OrderAscending sends the oldest first, draining a backlog in start order.
	OrderAscending = ffi.MoqOrderAscending
	// OrderDescending sends the newest first, so congestion sheds the backlog rather than the live edge.
	OrderDescending = ffi.MoqOrderDescending
)

// DeflateCompression makes each group one DEFLATE stream, which suits small, repetitive frames like JSON.
func DeflateCompression() *Compression {
	return ffi.MoqCompressionDeflate()
}

// BBRCongestionControl is BBR v3, the default: it keeps queues short and the send rate steady.
func BBRCongestionControl() *CongestionControl {
	return ffi.MoqCongestionControlBbr()
}

// CubicCongestionControl is CUBIC: it chases throughput, so the send rate sawtooths.
func CubicCongestionControl() *CongestionControl {
	return ffi.MoqCongestionControlCubic()
}

// UnlimitedCongestionControl runs no congestion control and no pacing, for tests and controlled links.
func UnlimitedCongestionControl() *CongestionControl {
	return ffi.MoqCongestionControlUnlimited()
}

// ParseInstance names an instance with text, failing with ErrInvalidInstance when it is empty.
func ParseInstance(text string) (*Instance, error) {
	return ffi.MoqInstanceParse(text)
}

// Versions lists the protocol versions offered by default, most preferred
// first, spelled the way ClientConfig.Versions takes them.
func Versions() []string {
	return ffi.MoqVersions()
}

// LogLevel configures the native tracing log level (e.g. "info", "debug").
func LogLevel(level string) error {
	return ffi.MoqLogLevel(level)
}

// Protocol registries and recognized kinds carried by ProtocolError.
const (
	// ErrorScopeSession identifies a session close code.
	ErrorScopeSession = ffi.MoqErrorScopeSession
	// ErrorScopeStream identifies a stream reset code.
	ErrorScopeStream = ffi.MoqErrorScopeStream
	// ProtocolKindCancel identifies cancel.
	ProtocolKindCancel = ffi.MoqProtocolKindCancel
	// ProtocolKindInternal identifies internal.
	ProtocolKindInternal = ffi.MoqProtocolKindInternal
	// ProtocolKindUnauthorized identifies unauthorized.
	ProtocolKindUnauthorized = ffi.MoqProtocolKindUnauthorized
	// ProtocolKindExpired identifies an expired authorization token.
	ProtocolKindExpired = ffi.MoqProtocolKindExpired
	// ProtocolKindProtocolViolation identifies protocol violation.
	ProtocolKindProtocolViolation = ffi.MoqProtocolKindProtocolViolation
	// ProtocolKindGoawayTimeout identifies goaway timeout.
	ProtocolKindGoawayTimeout = ffi.MoqProtocolKindGoawayTimeout
	// ProtocolKindTimeout identifies timeout.
	ProtocolKindTimeout = ffi.MoqProtocolKindTimeout
	// ProtocolKindVersion identifies version.
	ProtocolKindVersion = ffi.MoqProtocolKindVersion
	// ProtocolKindDeliveryTimeout identifies delivery timeout.
	ProtocolKindDeliveryTimeout = ffi.MoqProtocolKindDeliveryTimeout
	// ProtocolKindGoingAway identifies going away.
	ProtocolKindGoingAway = ffi.MoqProtocolKindGoingAway
	// ProtocolKindTooFarBehind identifies too far behind.
	ProtocolKindTooFarBehind = ffi.MoqProtocolKindTooFarBehind
	// ProtocolKindMalformedTrack identifies malformed track.
	ProtocolKindMalformedTrack = ffi.MoqProtocolKindMalformedTrack
	// ProtocolKindNotFound identifies not found.
	ProtocolKindNotFound = ffi.MoqProtocolKindNotFound
	// ProtocolKindUnroutable identifies unroutable.
	ProtocolKindUnroutable = ffi.MoqProtocolKindUnroutable
	// ProtocolKindOld identifies old.
	ProtocolKindOld = ffi.MoqProtocolKindOld
	// ProtocolKindEvicted identifies evicted.
	ProtocolKindEvicted = ffi.MoqProtocolKindEvicted
	// ProtocolKindWrongSize identifies wrong size.
	ProtocolKindWrongSize = ffi.MoqProtocolKindWrongSize
	// ProtocolKindFrameTooLarge identifies frame too large.
	ProtocolKindFrameTooLarge = ffi.MoqProtocolKindFrameTooLarge
	// ProtocolKindTimestampMismatch identifies timestamp mismatch.
	ProtocolKindTimestampMismatch = ffi.MoqProtocolKindTimestampMismatch
	// ProtocolKindApp identifies app.
	ProtocolKindApp = ffi.MoqProtocolKindApp
	// ProtocolKindUnknown identifies unknown.
	ProtocolKindUnknown = ffi.MoqProtocolKindUnknown
	// ProtocolKindTooManyRequests identifies an exceeded session request cap.
	ProtocolKindTooManyRequests = ffi.MoqProtocolKindTooManyRequests
	// ProtocolKindMalformedAuthToken identifies an unparseable authorization token.
	ProtocolKindMalformedAuthToken = ffi.MoqProtocolKindMalformedAuthToken
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
