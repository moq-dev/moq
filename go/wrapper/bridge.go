package moq

import (
	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// Hand the subpackages (moq.dev/moq/json) the handles behind these types
// without exporting them.
func init() {
	bridge.BroadcastProducer = func(v any) *ffi.MoqBroadcastProducer { return v.(*BroadcastProducer).inner }
	bridge.TrackProducer = func(v any) *ffi.MoqTrackProducer { return v.(*TrackProducer).inner }
	bridge.TrackConsumer = func(v any) *ffi.MoqTrackConsumer { return v.(*TrackConsumer).inner }
	bridge.TrackDemand = func(inner *ffi.MoqTrackDemand) any { return &TrackDemand{inner: inner} }
}
