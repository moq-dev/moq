package video

import (
	"testing"
	"time"

	"moq.dev/moq"
	"moq.dev/moq/media"
)

// A negative join is refused before the generated converter would panic on it.
func TestRefusesNegativeJoin(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()
	catalog, err := media.NewCatalogProducer(broadcast)
	if err != nil {
		t.Fatal(err)
	}

	// Everything but the join is valid, so only the duration check can refuse it.
	negative := -time.Millisecond
	input := EncoderInput{Format: moq.RgbaVideoPixelFormat(), Width: 320, Height: 240, Framerate: FrameRate{Numerator: 30, Denominator: 1}}
	output := EncoderOutput{Codec: moq.H264VideoCodec(), Kind: moq.SoftwareEncoder(), Join: &negative}
	if _, err := NewProducer(catalog, ProducerOptions{Input: input, Output: output}); err == nil {
		t.Fatal("a producer opened with a negative join, want an error")
	}
	if _, err := NewEncoder(input, output); err == nil {
		t.Fatal("an encoder opened with a negative join, want an error")
	}
}
