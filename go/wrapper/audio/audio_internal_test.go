package audio

import (
	"testing"
	"time"
)

// Go's zero FrameDuration is the native null, the codec's own frame, and a
// negative one is refused before the generated converter would panic on it.
func TestEncoderOutputFrameDuration(t *testing.T) {
	raw, err := EncoderOutput{}.ffi()
	if err != nil || raw.FrameDuration != nil {
		t.Fatalf("zero frame duration = %v, %v; want nil", raw.FrameDuration, err)
	}

	raw, err = EncoderOutput{FrameDuration: 20 * time.Millisecond}.ffi()
	if err != nil || raw.FrameDuration == nil || *raw.FrameDuration != 20*time.Millisecond {
		t.Fatalf("frame duration = %v, %v; want 20ms", raw.FrameDuration, err)
	}

	if _, err := (EncoderOutput{FrameDuration: -time.Millisecond}).ffi(); err == nil {
		t.Fatal("a negative frame duration converted, want an error")
	}
}
