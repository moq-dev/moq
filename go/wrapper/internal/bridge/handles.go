package bridge

import ffi "moq.dev/moq-ffi/moq"

// The generated handles behind the root package's wrapper types, which it keeps
// unexported. Package moq sets these from init, and every subpackage imports
// moq, so they are set before a subpackage can call them. They take and return
// `any` because this package cannot import moq's types without a cycle.
var (
	// BroadcastProducer returns the handle behind a *moq.BroadcastProducer.
	BroadcastProducer func(any) *ffi.MoqBroadcastProducer
	// TrackProducer returns the handle behind a *moq.TrackProducer.
	TrackProducer func(any) *ffi.MoqTrackProducer
	// TrackConsumer returns the handle behind a *moq.TrackConsumer.
	TrackConsumer func(any) *ffi.MoqTrackConsumer
	// TrackDemand wraps a handle as a *moq.TrackDemand.
	TrackDemand func(*ffi.MoqTrackDemand) any
)
