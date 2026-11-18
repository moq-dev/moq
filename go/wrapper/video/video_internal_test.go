package video

import (
	"testing"
	"time"
)

// A negative join is refused before the generated converter would panic on it.
func TestRefusesNegativeJoin(t *testing.T) {
	negative := -time.Millisecond
	output := EncoderOutput{Join: &negative}
	if _, err := NewProducer(nil, ProducerOptions{Output: output}); err == nil {
		t.Fatal("a producer opened with a negative join, want an error")
	}
	if _, err := NewEncoder(EncoderInput{}, output); err == nil {
		t.Fatal("an encoder opened with a negative join, want an error")
	}
}
