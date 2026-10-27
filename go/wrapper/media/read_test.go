package media

import (
	ffi "moq.dev/moq-ffi/moq"
	"testing"
	"time"
)

func TestMediaReadPreservesFramesAndBreaks(t *testing.T) {
	frame, err := mediaRead(ffi.MoqMediaReadFrame{Frame: ffi.MoqMediaFrame{Payload: []byte("encoded"), TimestampUs: 123456, Keyframe: true}})
	if err != nil {
		t.Fatal(err)
	}
	value, ok := frame.(ReadFrame)
	if !ok || string(value.Frame.Payload) != "encoded" || value.Frame.Timestamp != 123456*time.Microsecond || !value.Frame.Keyframe {
		t.Fatalf("frame: %#v", frame)
	}
	gap, err := mediaRead(ffi.MoqMediaReadGap{TimestampUs: 123456, Duration: 78901 * time.Microsecond})
	if err != nil {
		t.Fatal(err)
	}
	if gap != (ReadGap{Timestamp: 123456 * time.Microsecond, Duration: 78901 * time.Microsecond}) {
		t.Fatalf("gap: %#v", gap)
	}
	end, err := mediaRead(ffi.MoqMediaReadEnd{EndUs: 202357})
	if err != nil {
		t.Fatal(err)
	}
	if end != (ReadEnd{End: 202357 * time.Microsecond}) {
		t.Fatalf("end: %#v", end)
	}
}
