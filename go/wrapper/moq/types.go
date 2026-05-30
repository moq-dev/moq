package moq

import ffi "github.com/moq-dev/moq-go-ffi/moq"

// Record and enum types re-exported from the ffi layer without the Moq prefix.
// These are plain data, so type aliases are exact: a moq.AudioFrame is an
// ffi.MoqAudioFrame, constructible and comparable across the boundary.
type (
	Audio              = ffi.MoqAudio
	AudioCodec         = ffi.MoqAudioCodec
	AudioDecoderOutput = ffi.MoqAudioDecoderOutput
	AudioEncoderInput  = ffi.MoqAudioEncoderInput
	AudioEncoderOutput = ffi.MoqAudioEncoderOutput
	AudioFormat        = ffi.MoqAudioFormat
	AudioFrame         = ffi.MoqAudioFrame
	Catalog            = ffi.MoqCatalog
	Dimensions         = ffi.MoqDimensions
	Frame              = ffi.MoqFrame
	Video              = ffi.MoqVideo

	// Container selects how subscribed media frames are demuxed. Construct one
	// of the variant types below.
	Container       = ffi.Container
	ContainerLegacy = ffi.ContainerLegacy
	ContainerCmaf   = ffi.ContainerCmaf
	ContainerLoc    = ffi.ContainerLoc

	// Session is the established connection. Hold it to keep the connection
	// alive; its methods (Closed, Cancel, Shutdown) block, so drive them from
	// a goroutine if you need cancellation.
	Session = ffi.MoqSession
)

// AudioFormat values: the raw PCM sample layout fed to or returned from the
// in-process Opus codec.
const (
	AudioFormatU8        = ffi.MoqAudioFormatU8
	AudioFormatS16       = ffi.MoqAudioFormatS16
	AudioFormatS32       = ffi.MoqAudioFormatS32
	AudioFormatF32       = ffi.MoqAudioFormatF32
	AudioFormatU8Planar  = ffi.MoqAudioFormatU8Planar
	AudioFormatS16Planar = ffi.MoqAudioFormatS16Planar
	AudioFormatS32Planar = ffi.MoqAudioFormatS32Planar
	AudioFormatF32Planar = ffi.MoqAudioFormatF32Planar
)

// AudioCodecOpus is the only codec currently supported for raw audio tracks.
const AudioCodecOpus = ffi.MoqAudioCodecOpus

// LogLevel configures the native tracing log level (e.g. "info", "debug").
func LogLevel(level string) error {
	return ffi.MoqLogLevel(level)
}
