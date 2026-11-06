package moq

import (
	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// Hand the layer subpackages the handles behind these types
// without exporting them.
func init() {
	bridge.OriginProducer = func(v any) *ffi.MoqOriginProducer { return v.(*OriginProducer).inner }
	bridge.WrapGroupProducer = func(v *ffi.MoqGroupProducer) any { return &GroupProducer{inner: v} }
	bridge.WrapGroupConsumer = func(v *ffi.MoqGroupConsumer) any { return &GroupConsumer{inner: v} }
	bridge.WrapTrackProducer = func(v *ffi.MoqTrackProducer) any { return &TrackProducer{inner: v} }
	bridge.WrapTrackConsumer = func(v *ffi.MoqTrackConsumer) any { return &TrackConsumer{inner: v} }
	bridge.TrackInfo = func(v any) (*ffi.MoqTrackInfo, error) { return trackInfoFFI(v.(*TrackInfo)) }

	bridge.BroadcastConsumer = func(v any) *ffi.MoqBroadcastConsumer { return v.(*BroadcastConsumer).inner }
	bridge.TrackRequest = func(v any) *ffi.MoqTrackRequest { return v.(*TrackRequest).inner }
	bridge.Subscription = func(v any) (*ffi.MoqSubscription, error) { return subscriptionFFI(v.(*Subscription)) }
	bridge.Delay = func(v any) (ffi.MoqDelay, error) { return v.(Delay).ffi() }

	bridge.BroadcastProducer = func(v any) *ffi.MoqBroadcastProducer { return v.(*BroadcastProducer).inner }
	bridge.TrackProducer = func(v any) *ffi.MoqTrackProducer { return v.(*TrackProducer).inner }
	bridge.TrackConsumer = func(v any) *ffi.MoqTrackConsumer { return v.(*TrackConsumer).inner }
	bridge.GroupConsumer = func(v any) *ffi.MoqGroupConsumer { return v.(*GroupConsumer).inner }
	bridge.TrackDemand = func(inner *ffi.MoqTrackDemand) any { return &TrackDemand{inner: inner} }
	bridge.Allocator = func(v any) *ffi.MoqAllocator { return v.(*Allocator).inner }
	bridge.Reservation = func(inner *ffi.MoqReservation) any { return &Reservation{inner: inner} }
	bridge.VideoDecodedFrame = func(inner *ffi.MoqVideoDecodedFrame) any { return &VideoDecodedFrame{inner: inner} }
}
