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

// FetchGroup fetches one complete group by track name and group start
// without holding a live subscription.
func (b *BroadcastConsumer) FetchGroup(
	ctx context.Context,
	name string,
	start uint64,
	options *FetchGroupOptions,
) (*GroupConsumer, error) {
	inner, err := b.inner.FetchGroup(ctx, name, start, options)
	if err != nil {
		return nil, err
	}
	return &GroupConsumer{inner: inner}, nil
}

// Resolve returns the broadcast serving a catalog rendition, built with
// media.VideoRendition or media.AudioRendition. The rendition's Broadcast reference, nil or
// empty, names this broadcast; anything else names a sibling relative to it (e.g. "./source").
// Its Identity is the stream the reference names: a broadcast proven to serve another one
// errors instead of resolving, while a route over a wire that carries no identity cannot prove
// one, so there it is best effort. Call it on a rendition that carries a reference before SubscribeTrack or
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

// Start is this group's start within the track.
func (g *GroupConsumer) Start() uint64 {
	return g.inner.Start()
}

// ReadFrame returns the next raw frame, or (nil, nil) when the group ends.
func (g *GroupConsumer) ReadFrame(ctx context.Context) (*Frame, error) {
	return bridge.Call(ctx, nil, g.inner.ReadFrame)
}

// All ranges over raw frames until the group ends or the loop breaks.
func (g *GroupConsumer) All(ctx context.Context) iter.Seq2[*Frame, error] {
	return bridge.Seq(ctx, g.inner.Cancel, g.ReadFrame)
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

// RecvGroup returns the next group in arrival order (possibly out of start order),
// or (nil, nil) when the track ends. Groups already held come in the
// subscription's group order, newest first unless it asks ascending. Prefer this
// for live, latency-sensitive consumption.
func (t *TrackConsumer) RecvGroup(ctx context.Context) (*GroupConsumer, error) {
	return t.group(ctx, t.inner.RecvGroup)
}

// NextGroup returns the next group in start order, skipping forward if
// behind, or (nil, nil) when the track ends. Prefer this when order matters
// more than latency. Shares the ordered cursor with ReadFrame: a group one
// method has already taken is not returned by the other.
func (t *TrackConsumer) NextGroup(ctx context.Context) (*GroupConsumer, error) {
	return t.group(ctx, t.inner.NextGroup)
}

func (t *TrackConsumer) group(ctx context.Context, next func(context.Context) (**ffi.MoqGroupConsumer, error)) (*GroupConsumer, error) {
	res, err := bridge.CallHandle(ctx, nil, func(ctx context.Context) (*ffi.MoqGroupConsumer, error) {
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
func (t *TrackConsumer) ReadFrame(ctx context.Context) (*Frame, error) {
	return bridge.Call(ctx, nil, t.inner.ReadFrame)
}

// Info returns the publisher-side track properties learned during subscription.
func (t *TrackConsumer) Info() TrackInfo {
	return trackInfoFromFFI(t.inner.Info())
}

// Update changes this subscriber's delivery preferences. It fails on a
// negative delay, or a range time the track can't serve.
func (t *TrackConsumer) Update(subscription Subscription) error {
	sub, err := subscription.ffi()
	if err != nil {
		return err
	}
	return t.inner.Update(sub)
}

// Refresh asks the publisher for a group newer than the newest held, and
// returns once the request is sent. It is judged by the track's TrackRefresh:
// sent once the newest group is old enough, when whatever serves the track
// reads it (the session forwarding it upstream, or a local publisher blocked in
// TrackDemand.Refresh). Concurrent calls share one request and all return when
// it is sent. Cancelling ctx before then withdraws this call's request. It
// returns at once when there is nothing to send, or when a newer group lands
// first. The new group arrives through the normal reads, such as NextGroup.
func (t *TrackConsumer) Refresh(ctx context.Context) error {
	return bridge.CallErr(ctx, nil, t.inner.Refresh)
}

// Groups ranges over groups in start order.
func (t *TrackConsumer) Groups(ctx context.Context) iter.Seq2[*GroupConsumer, error] {
	return bridge.Seq(ctx, t.inner.Cancel, t.NextGroup)
}

// GroupsAsArrived ranges over groups in arrival order, including
// out-of-order deliveries. Groups already held come in the subscription's
// order, newest first unless it asks ascending, as RecvGroup returns them.
func (t *TrackConsumer) GroupsAsArrived(ctx context.Context) iter.Seq2[*GroupConsumer, error] {
	return bridge.Seq(ctx, t.inner.Cancel, t.RecvGroup)
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

// Timestamp is the presentation timestamp.
func (f *VideoDecodedFrame) Timestamp() Timestamp {
	return f.inner.Timestamp()
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
func (f *VideoDecodedFrame) Pixels(format *VideoPixelFormat) ([]byte, error) {
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

type (
	// VideoSurface is a decoded frame's platform surface: VideoSurfacePixelBuffer on macOS and
	// iOS, or VideoSurfaceUnknown for a kind this version cannot name yet.
	VideoSurface = ffi.MoqVideoSurface
	// VideoSurfacePixelBuffer is an Apple CVPixelBufferRef (IOSurface-backed NV12), as the address in Pointer.
	VideoSurfacePixelBuffer = ffi.MoqVideoSurfacePixelBuffer
	// VideoSurfaceUnknown is a surface kind this version of the package cannot name yet.
	VideoSurfaceUnknown = ffi.MoqVideoSurfaceUnknown
)

// Close releases the frame's surface back to the decoder.
func (f *VideoDecodedFrame) Close() {
	f.inner.Destroy()
}
