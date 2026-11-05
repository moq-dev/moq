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
	// Allocator returns the handle behind a *moq.Allocator.
	Allocator func(any) *ffi.MoqAllocator
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

// Subscription converts a *moq.Subscription to the generated record.
var Subscription func(any) (*ffi.MoqSubscription, error)

// Delay converts a moq.Delay to the generated record.
var Delay func(any) (ffi.MoqDelay, error)

// NonNegative refuses a negative duration, which the generated converter
// would panic on rather than return an error.
func NonNegative(value *time.Duration, name string) error {
	if value != nil && *value < 0 {
		return fmt.Errorf("negative %s: %v", name, *value)
	}
	return nil
}

// Rendition refuses a rendition's negative jitter or warmup, for the same reason.
func Rendition(rendition ffi.MoqRendition) error {
	switch rendition := rendition.(type) {
	case ffi.MoqRenditionAudio:
		return Audio(rendition.Config)
	case ffi.MoqRenditionVideo:
		return Video(rendition.Config)
	}
	return nil
}

// Audio refuses an audio rendition's negative jitter or warmup.
func Audio(audio ffi.MoqAudio) error {
	if err := NonNegative(audio.Jitter, "jitter"); err != nil {
		return err
	}
	return NonNegative(audio.Warmup, "warmup")
}

// Video refuses a video rendition's negative warmup.
func Video(video ffi.MoqVideo) error {
	return NonNegative(video.Warmup, "warmup")
}
