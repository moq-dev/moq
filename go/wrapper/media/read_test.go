package media

import (
	"testing"
	"time"

	"moq.dev/moq"
	ffi "moq.dev/moq-ffi/moq"
)

func TestMediaReadPreservesFramesAndBreaks(t *testing.T) {
	tick := moq.Timestamp{Value: 1, Scale: 90_000}
	frame, err := mediaRead(ffi.MoqMediaReadFrame{Frame: ffi.MoqMediaFrame{Payload: []byte("encoded"), Timestamp: tick, Keyframe: true}})
	if err != nil {
		t.Fatal(err)
	}
	value, ok := frame.(ReadFrame)
	if !ok || string(value.Frame.Payload) != "encoded" || value.Frame.Timestamp != tick || !value.Frame.Keyframe {
		t.Fatalf("frame: %#v", frame)
	}
	gap, err := mediaRead(ffi.MoqMediaReadGap{Timestamp: tick, Duration: 78901 * time.Microsecond})
	if err != nil {
		t.Fatal(err)
	}
	if gap != (ReadGap{Timestamp: tick, Duration: 78901 * time.Microsecond}) {
		t.Fatalf("gap: %#v", gap)
	}
	end, err := mediaRead(ffi.MoqMediaReadEnd{End: tick})
	if err != nil {
		t.Fatal(err)
	}
	if end != (ReadEnd{End: tick}) {
		t.Fatalf("end: %#v", end)
	}
}
