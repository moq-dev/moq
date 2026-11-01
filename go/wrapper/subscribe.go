package moq

import (
	"context"
	"iter"

	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// BroadcastConsumer reads tracks from a broadcast.
type BroadcastConsumer struct {
	inner *ffi.MoqBroadcastConsumer
}

// Closed blocks until the broadcast ends: its publisher closed it, or every producer
// left. Cancelling ctx stops waiting.
func (b *BroadcastConsumer) Closed(ctx context.Context) error {
	return b.inner.Closed(ctx)
}

// SubscribeTrack subscribes to a track, receiving arbitrary byte payloads.
// subscription tunes delivery priority, group range, and staleness; pass nil for defaults.
func (b *BroadcastConsumer) SubscribeTrack(
	ctx context.Context,
	name string,
	subscription *Subscription,
) (*TrackConsumer, error) {
	sub, err := subscriptionFFI(subscription)
	if err != nil {
		return nil, err
	}
	inner, err := b.inner.SubscribeTrack(ctx, name, sub)
	if err != nil {
		return nil, err
	}
	return &TrackConsumer{inner: inner}, nil
}

// FetchGroup fetches one complete group by track name and group sequence
// without holding a live subscription.
func (b *BroadcastConsumer) FetchGroup(
	ctx context.Context,
	name string,
	sequence uint64,
	options *FetchGroupOptions,
) (*GroupConsumer, error) {
	if options != nil {
		if err := bridge.NonNegative(options.MaxDelay, "max delay"); err != nil {
			return nil, err
		}
	}
	inner, err := b.inner.FetchGroup(ctx, name, sequence, options)
	if err != nil {
		return nil, err
	}
	return &GroupConsumer{inner: inner}, nil
}

// Resolve returns the broadcast serving a catalog rendition, built with
// media.VideoRendition or media.AudioRendition. The rendition's Broadcast reference, nil or
// empty, names this broadcast; anything else names a sibling relative to it (e.g. "./source").
// Its Epoch is the publisher instance the reference names: a broadcast proven to serve another
// instance errors instead of resolving, and a route without an epoch cannot prove one, so there
// it is best effort. Call it on a rendition that carries a reference before SubscribeTrack or
// FetchGroup, which take a track name rather than a rendition: the rendition's Track, or its ID
// when nil. The rendition-based media.NewTrackConsumer, media.FetchGroup, audio.Subscribe, and
// video.Subscribe resolve it themselves.
//
// Errors if this broadcast came from a local producer rather than an origin, since a
// standalone broadcast has no sibling to name.
func (b *BroadcastConsumer) Resolve(ctx context.Context, rendition ffi.MoqRendition) (*BroadcastConsumer, error) {
	if err := bridge.Rendition(rendition); err != nil {
		return nil, err
	}
	inner, err := b.inner.Resolve(ctx, rendition)
	if err != nil {
		return nil, err
	}
	return &BroadcastConsumer{inner: inner}, nil
}

// GroupConsumer is a stream of raw frames within a single group.
type GroupConsumer struct {
	inner *ffi.MoqGroupConsumer
}

// Sequence is this group's sequence number within the track.
func (g *GroupConsumer) Sequence() uint64 {
	return g.inner.Sequence()
}

// ReadFrame returns the next raw frame, or (nil, nil) when the group ends.
func (g *GroupConsumer) ReadFrame(ctx context.Context) (*Frame, error) {
	frame, err := bridge.Call(ctx, g.inner.Cancel, g.inner.ReadFrame)
	return frameFromFFI(frame), err
}

// All ranges over raw frames until the group ends or the loop breaks.
func (g *GroupConsumer) All(ctx context.Context) iter.Seq2[*Frame, error] {
	return bridge.Seq(ctx, g.ReadFrame)
}

// Cancel stops the stream.
func (g *GroupConsumer) Cancel() {
	g.inner.Cancel()
}

// TrackConsumer is a stream of groups from a track. Each group is itself a
// stream of raw frames.
type TrackConsumer struct {
	inner *ffi.MoqTrackConsumer
}

// RecvGroup returns the next group in arrival order (possibly out of sequence),
// or (nil, nil) when the track ends. Prefer this for live, latency-sensitive
// consumption.
func (t *TrackConsumer) RecvGroup(ctx context.Context) (*GroupConsumer, error) {
	return t.group(ctx, t.inner.RecvGroup)
}

// NextGroup returns the next group in sequence order, skipping forward if
// behind, or (nil, nil) when the track ends. Prefer this when order matters
// more than latency. Shares the sequence cursor with ReadFrame: a group one
// method has already taken is not returned by the other.
func (t *TrackConsumer) NextGroup(ctx context.Context) (*GroupConsumer, error) {
	return t.group(ctx, t.inner.NextGroup)
}

func (t *TrackConsumer) group(ctx context.Context, next func(context.Context) (**ffi.MoqGroupConsumer, error)) (*GroupConsumer, error) {
	res, err := bridge.CallHandle(ctx, t.inner.Cancel, func(ctx context.Context) (*ffi.MoqGroupConsumer, error) {
		res, err := next(ctx)
		if err != nil || res == nil {
			return nil, err
		}
		return *res, nil
	})
	if err != nil || res == nil {
		return nil, err
	}
	return &GroupConsumer{inner: res}, nil
}

// ReadFrame reads the first raw frame of the next group, or (nil, nil)
// when the track ends. Convenient for one-frame-per-group tracks. Completed
// empty groups are skipped; a nil frame is track EOF, not an empty group.
// Cancelling the context cancels this consumer, not just this call: see
// package docs.
func (t *TrackConsumer) ReadFrame(ctx context.Context) (*Frame, error) {
	frame, err := bridge.Call(ctx, t.inner.Cancel, t.inner.ReadFrame)
	return frameFromFFI(frame), err
}

// RecvDatagram returns the next best-effort datagram in arrival order, or
// (nil, nil) when the track ends.
func (t *TrackConsumer) RecvDatagram(ctx context.Context) (*Datagram, error) {
	datagram, err := bridge.Call(ctx, t.inner.Cancel, t.inner.RecvDatagram)
	return datagramFromFFI(datagram), err
}

// Info returns the publisher-side track properties learned during subscription.
func (t *TrackConsumer) Info() TrackInfo {
	return trackInfoFromFFI(t.inner.Info())
}

// Update changes this subscriber's delivery preferences. It fails on a
// negative MaxDelay, or a time anchor the track can't serve.
func (t *TrackConsumer) Update(subscription Subscription) error {
	sub, err := subscription.ffi()
	if err != nil {
		return err
	}
	return t.inner.Update(sub)
}

// Groups ranges over groups in sequence order.
func (t *TrackConsumer) Groups(ctx context.Context) iter.Seq2[*GroupConsumer, error] {
	return bridge.Seq(ctx, t.NextGroup)
}

// GroupsAsArrived ranges over groups in arrival order, including
// out-of-sequence deliveries.
func (t *TrackConsumer) GroupsAsArrived(ctx context.Context) iter.Seq2[*GroupConsumer, error] {
	return bridge.Seq(ctx, t.RecvGroup)
}

// Datagrams ranges over best-effort datagrams in arrival order.
func (t *TrackConsumer) Datagrams(ctx context.Context) iter.Seq2[*Datagram, error] {
	return bridge.Seq(ctx, t.RecvDatagram)
}

// Cancel stops the stream.
func (t *TrackConsumer) Cancel() {
	t.inner.Cancel()
}

// VideoDecodedFrame is one decoded video frame. It owns the decoder's surface
// until Close, which returns it to the decoder: holding many frames stalls
// decoding. It stays valid after its video.Consumer is cancelled.
type VideoDecodedFrame struct {
	inner *ffi.MoqVideoDecodedFrame
}

// TimestampUs is the presentation timestamp in microseconds.
func (f *VideoDecodedFrame) TimestampUs() uint64 {
	return f.inner.TimestampUs()
}

// Width is the decoded width in pixels.
func (f *VideoDecodedFrame) Width() uint32 {
	return f.inner.Width()
}

// Height is the decoded height in pixels.
func (f *VideoDecodedFrame) Height() uint32 {
	return f.inner.Height()
}

// Pixels converts the frame to tightly packed pixels in format.
func (f *VideoDecodedFrame) Pixels(format VideoPixelFormat) ([]byte, error) {
	return f.inner.Pixels(format)
}

// Surface borrows the decoder's surface, or returns nil when the frame is in CPU
// memory. Only a video.Consumer with DecoderOutput.Surface set produces one,
// and it stays valid until Close.
func (f *VideoDecodedFrame) Surface() VideoSurface {
	if surface := f.inner.Surface(); surface != nil {
		return *surface
	}
	return nil
}

// Close releases the frame's surface back to the decoder.
func (f *VideoDecodedFrame) Close() {
	f.inner.Destroy()
}
