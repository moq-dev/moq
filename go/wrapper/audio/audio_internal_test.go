package audio

import (
	"testing"
	"time"
)

func TestEncoderOutputRejectsOverflowingFrameDuration(t *testing.T) {
	out := EncoderOutput{FrameDuration: (1<<32 + 20_000) * time.Microsecond}
	if _, err := out.ffi(); err == nil {
		t.Fatal("a frame duration past uint32 microseconds must fail, not truncate")
	}

	out.FrameDuration = 20 * time.Millisecond
	raw, err := out.ffi()
	if err != nil {
		t.Fatal(err)
	}
	if raw.FrameDurationUs != 20_000 {
		t.Fatalf("frame duration = %d us, want 20000", raw.FrameDurationUs)
	}
}
