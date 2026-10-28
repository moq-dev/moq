package moq

import (
	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// Hand the layer subpackages the handles behind these types
// without exporting them.
func init() {
	bridge.BroadcastConsumer = func(v any) *ffi.MoqBroadcastConsumer { return v.(*BroadcastConsumer).inner }
	bridge.TrackRequest = func(v any) *ffi.MoqTrackRequest { return v.(*TrackRequest).inner }
	bridge.Frame = func(v any) (ffi.MoqFrame, error) { return v.(Frame).ffi() }
	bridge.Subscription = func(v any) (*ffi.MoqSubscription, error) { return subscriptionFFI(v.(*Subscription)) }

	bridge.BroadcastProducer = func(v any) *ffi.MoqBroadcastProducer { return v.(*BroadcastProducer).inner }
	bridge.TrackProducer = func(v any) *ffi.MoqTrackProducer { return v.(*TrackProducer).inner }
	bridge.TrackConsumer = func(v any) *ffi.MoqTrackConsumer { return v.(*TrackConsumer).inner }
	bridge.GroupConsumer = func(v any) *ffi.MoqGroupConsumer { return v.(*GroupConsumer).inner }
	bridge.TrackDemand = func(inner *ffi.MoqTrackDemand) any { return &TrackDemand{inner: inner} }
	bridge.Allocator = func(v any) *ffi.MoqAllocator { return v.(*Allocator).inner }
	bridge.Reservation = func(inner *ffi.MoqReservation) any { return &Reservation{inner: inner} }
	bridge.VideoDecodedFrame = func(inner *ffi.MoqVideoDecodedFrame) any { return &VideoDecodedFrame{inner: inner} }
}
