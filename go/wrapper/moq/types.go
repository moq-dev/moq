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
	ConnectionStats    = ffi.MoqConnectionStats
	Datagram           = ffi.MoqDatagram
	Dimensions         = ffi.MoqDimensions
	Frame              = ffi.MoqFrame
	MediaFrame         = ffi.MoqMediaFrame
	FetchGroupOptions  = ffi.MoqFetchGroupOptions
	OriginOptions      = ffi.MoqOriginOptions
	Route              = ffi.MoqRoute
	Subscription       = ffi.MoqSubscription
	TrackInfo          = ffi.MoqTrackInfo
	Video              = ffi.MoqVideo
	VideoHint          = ffi.MoqVideoHint
	VideoPresentation  = ffi.MoqVideoPresentation

	// Container selects how subscribed media frames are demuxed. Build one with
	// LegacyContainer, CmafContainer, or LocContainer.
	Container       = ffi.MoqContainer
	ContainerLegacy = ffi.MoqContainerLegacy
	ContainerCmaf   = ffi.MoqContainerCmaf
	ContainerLoc    = ffi.MoqContainerLoc
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
