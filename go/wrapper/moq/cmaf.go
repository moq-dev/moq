package moq

import ffi "github.com/moq-dev/moq-go-ffi/moq"

// CMAFOutput contains initialization and media data produced for one frame batch.
type CMAFOutput struct {
	// Initialization is the current init segment, or nil until codec metadata is available.
	Initialization []byte
	// Fragment is one media fragment, or nil when the batch produced no usable samples.
	Fragment []byte
}

// CMAFMuxer packages one encoded audio or video rendition as CMAF.
type CMAFMuxer struct {
	inner *ffi.MoqCmafMuxer
}

// NewVideoCMAFMuxer creates a video muxer whose output timestamps are relative to originUS.
func NewVideoCMAFMuxer(video Video, originUS uint64) (*CMAFMuxer, error) {
	inner, err := ffi.NewMoqCmafMuxer(ffi.MoqCmafConfig{
		Track:    ffi.MoqCmafTrackVideo{Config: video},
		OriginUs: originUS,
	})
	if err != nil {
		return nil, err
	}
	return &CMAFMuxer{inner: inner}, nil
}

// NewAudioCMAFMuxer creates an audio muxer whose output timestamps are relative to originUS.
func NewAudioCMAFMuxer(audio Audio, originUS uint64) (*CMAFMuxer, error) {
	inner, err := ffi.NewMoqCmafMuxer(ffi.MoqCmafConfig{
		Track:    ffi.MoqCmafTrackAudio{Config: audio},
		OriginUs: originUS,
	})
	if err != nil {
		return nil, err
	}
	return &CMAFMuxer{inner: inner}, nil
}

// Initialization returns the current init segment, or nil until inline codec metadata arrives.
func (m *CMAFMuxer) Initialization() ([]byte, error) {
	initialization, err := m.inner.InitSegment()
	if err != nil || initialization == nil {
		return nil, err
	}
	return *initialization, nil
}

// Mux encodes one codec-configuration-consistent batch on the configured presentation timeline.
func (m *CMAFMuxer) Mux(sequence uint32, frames []MediaFrame) (CMAFOutput, error) {
	output, err := m.inner.Mux(sequence, frames)
	if err != nil {
		return CMAFOutput{}, err
	}
	return CMAFOutput{
		Initialization: optionalBytes(output.Initialization),
		Fragment:       optionalBytes(output.Fragment),
	}, nil
}

// Close releases the native muxer.
func (m *CMAFMuxer) Close() {
	m.inner.Destroy()
}

func optionalBytes(value *[]byte) []byte {
	if value == nil {
		return nil
	}
	return *value
}
