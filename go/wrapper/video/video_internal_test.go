package video

import (
	"context"
	"testing"
	"time"

	"moq.dev/moq/media"
)

// The generated converter panics on a negative duration, so it is refused first.
func TestSubscribeRefusesNegativeWarmup(t *testing.T) {
	negative := -time.Millisecond
	if _, err := Subscribe(context.Background(), nil, "video", media.Video{Warmup: &negative}, DecoderOutput{}); err == nil {
		t.Fatal("a negative warmup was subscribed, want an error")
	}
}
