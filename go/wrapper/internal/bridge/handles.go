package bridge

import (
	"fmt"
	"time"

	ffi "moq.dev/moq-ffi/moq"
)

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
	// GroupConsumer returns the handle behind a *moq.GroupConsumer.
	GroupConsumer func(any) *ffi.MoqGroupConsumer
	// TrackDemand wraps a handle as a *moq.TrackDemand.
	TrackDemand func(*ffi.MoqTrackDemand) any
	// Bandwidth returns the handle behind a *moq.Bandwidth.
	Bandwidth func(any) *ffi.MoqBandwidth
	// Reservation wraps a native reservation as *moq.Reservation.
	Reservation func(*ffi.MoqReservation) any
	// CatalogProducer returns the handle behind a *media.CatalogProducer.
	CatalogProducer func(any) *ffi.MoqMediaCatalogProducer
	// VideoDecodedFrame wraps a decoded frame as a *moq.VideoDecodedFrame.
	VideoDecodedFrame func(*ffi.MoqVideoDecodedFrame) any
)

// BroadcastConsumer returns the handle behind a *moq.BroadcastConsumer.
var BroadcastConsumer func(any) *ffi.MoqBroadcastConsumer

// TrackRequest returns the handle behind a *moq.TrackRequest.
var TrackRequest func(any) *ffi.MoqTrackRequest

// Frame converts a moq.Frame to the generated record.
var Frame func(any) (ffi.MoqFrame, error)

// Subscription converts a *moq.Subscription to the generated record.
var Subscription func(any) (*ffi.MoqSubscription, error)

// Duration converts d to microseconds, refusing a negative duration rather
// than letting it wrap when cast to uint64.
func Duration(d time.Duration, name string) (uint64, error) {
	if d < 0 {
		return 0, fmt.Errorf("negative %s: %v", name, d)
	}
	return uint64(d.Microseconds()), nil
}

// NonNegative refuses a negative duration, which the generated converter
// would panic on rather than return an error.
func NonNegative(value *time.Duration, name string) error {
	if value != nil && *value < 0 {
		return fmt.Errorf("negative %s: %v", name, *value)
	}
	return nil
}

// OptionalDuration converts a caller's time to microseconds, preserving absence.
func OptionalDuration(value *time.Duration, name string) (*uint64, error) {
	if value == nil {
		return nil, nil
	}
	us, err := Duration(*value, name)
	if err != nil {
		return nil, err
	}
	return &us, nil
}
