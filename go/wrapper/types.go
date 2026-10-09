package moq

import ffi "moq.dev/moq-ffi/moq"

// Record, enum, and small object types re-exported from the ffi layer without the Moq prefix.
// These are plain data, so type aliases are exact: a moq.AudioFrame is an
// ffi.MoqAudioFrame, constructible and comparable across the boundary.
type (
	// AudioCodec selects the audio encoder codec. Build one with OpusAudioCodec or AacAudioCodec;
	// adding a codec later adds a constructor, not a breaking enum change.
	AudioCodec = ffi.MoqAudioCodec
	// AudioDecoderOutput configures the PCM format, sample rate, and channels DecodeAudio delivers.
	AudioDecoderOutput = ffi.MoqAudioDecoderOutput
	// AudioEncoderInput declares the PCM sample format, sample rate, and channel count of frames written to an audio producer.
	AudioEncoderInput = ffi.MoqAudioEncoderInput
	// AudioEncoderOutput configures the encoder: codec, optional sample rate, channels, bitrate, and frame duration.
	AudioEncoderOutput = ffi.MoqAudioEncoderOutput
	// AudioSampleFormat is a raw PCM sample layout, mirroring WebCodecs AudioData.format.
	AudioSampleFormat = ffi.MoqAudioSampleFormat
	// AudioFrame is one audio frame: PCM payload plus a presentation timestamp in microseconds.
	AudioFrame = ffi.MoqAudioFrame
	// ConnectionStatus is a connection lifecycle transition reported by Session.Status.
	ConnectionStatus = ffi.MoqConnectionStatus
	// ErrorScope is whether a protocol code is from the session or stream registry.
	ErrorScope = ffi.MoqErrorScope
	// FetchGroupOptions configures a single FetchGroup call, currently just the delivery priority.
	FetchGroupOptions = ffi.MoqFetchGroupOptions
	// ProtocolKind is a recognized protocol kind, or App / Unknown when the code is not named.
	ProtocolKind = ffi.MoqProtocolKind
	// OriginConfig configures a new origin, such as its maximum cache size in bytes.
	OriginConfig = ffi.MoqOriginConfig
	// Route is the hop chain a broadcast takes to reach an origin, and its static production and link cost (lower wins).
	Route = ffi.MoqRoute
	// Announce is a route over a prefix: the origin-relative Prefix, what each filter
	// wildcard matched (nil Captures for a route that only overlaps the scope), and the
	// Route serving it. It carries no broadcast; resolve a path with
	// [OriginConsumer.RequestBroadcast].
	Announce = ffi.MoqAnnounce
	// AnnounceEvent is what an AnnounceConsumer yields: AnnounceEventStart,
	// AnnounceEventUpdate, or AnnounceEventEnd.
	AnnounceEvent = ffi.MoqAnnounceEvent
	// AnnounceEventStart reports a route now covering a prefix that had none.
	AnnounceEventStart = ffi.MoqAnnounceEventStart
	// AnnounceEventUpdate reports the route covering a prefix changing hops or cost.
	AnnounceEventUpdate = ffi.MoqAnnounceEventUpdate
	// AnnounceEventEnd reports that no route covers a prefix any more, carrying its last route.
	AnnounceEventEnd = ffi.MoqAnnounceEventEnd
	// VideoDecoderOutput configures what DecodeVideo delivers: an optional resize, a max delay, and whether frames keep the decoder's surface (macOS only; refused elsewhere).
	VideoDecoderOutput = ffi.MoqVideoDecoderOutput
	// VideoSurface is a decoded frame's platform surface, from VideoDecodedFrame.Surface: VideoSurfacePixelBuffer on macOS and iOS.
	VideoSurface = ffi.MoqVideoSurface
	// VideoSurfacePixelBuffer is an Apple CVPixelBufferRef (IOSurface-backed NV12), as the address in Pointer.
	VideoSurfacePixelBuffer = ffi.MoqVideoSurfacePixelBuffer
	// VideoCodec identifies a published video track's codec: H.264 or H.265.
	VideoCodec = ffi.MoqVideoCodec
	// VideoPixelFormat is a CPU pixel layout (I420 or RGBA): written to a VideoProducer, or read from a VideoDecodedFrame.
	VideoPixelFormat = ffi.MoqVideoPixelFormat
	// VideoEncoderInput declares the pixel layout, resolution, and framerate of frames written to a video producer.
	VideoEncoderInput = ffi.MoqVideoEncoderInput
	// VideoEncoderOutput configures the video track name, codec, bitrate, keyframe interval, and backend preference.
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
)

// OpusAudioCodec selects Opus (RFC 6716) for EncodeAudio.
func OpusAudioCodec() *AudioCodec {
	return ffi.MoqAudioCodecOpus()
}

// AacAudioCodec selects AAC-LC through the platform's encoder for EncodeAudio.
// A host without one refuses it. Leave FrameDurationUs at 0 for AAC's own frame.
func AacAudioCodec() *AudioCodec {
	return ffi.MoqAudioCodecAac()
}

// VideoPixelFormat values: the raw pixel layout fed to the in-process encoder,
// and the one the in-process decoder delivers.
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
// "videotoolbox" (macOS, iOS), "mediafoundation" (Windows), or "openh264"
// (software, everywhere). Naming one this build lacks fails with a no-encoder
// error.
func NamedEncoder(name string) VideoEncoderKind {
	return VideoEncoderKindNamed{Name: name}
}

// ConnectionStatus values: the lifecycle of a client session's connection.
const (
	// ConnectionStatusConnected means a session connected (the first connect, or a reconnect after a drop).
	ConnectionStatusConnected = ffi.MoqConnectionStatusConnected
	// ConnectionStatusDisconnected means the session dropped; a reconnect attempt follows.
	ConnectionStatusDisconnected = ffi.MoqConnectionStatusDisconnected
	// ConnectionStatusMigrating means the peer sent a GOAWAY; the replacement is being dialed while the old session keeps serving.
	ConnectionStatusMigrating = ffi.MoqConnectionStatusMigrating
)

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
	// ProtocolKindProtocolViolation identifies protocol violation.
	ProtocolKindProtocolViolation = ffi.MoqProtocolKindProtocolViolation
	// ProtocolKindKeyValueFormatting identifies key value formatting.
	ProtocolKindKeyValueFormatting = ffi.MoqProtocolKindKeyValueFormatting
	// ProtocolKindGoawayTimeout identifies goaway timeout.
	ProtocolKindGoawayTimeout = ffi.MoqProtocolKindGoawayTimeout
	// ProtocolKindTimeout identifies timeout.
	ProtocolKindTimeout = ffi.MoqProtocolKindTimeout
	// ProtocolKindVersion identifies version.
	ProtocolKindVersion = ffi.MoqProtocolKindVersion
	// ProtocolKindDeliveryTimeout identifies delivery timeout.
	ProtocolKindDeliveryTimeout = ffi.MoqProtocolKindDeliveryTimeout
	// ProtocolKindSessionClosed identifies session closed.
	ProtocolKindSessionClosed = ffi.MoqProtocolKindSessionClosed
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
