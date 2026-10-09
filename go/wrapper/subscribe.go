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
	inner, err := b.inner.FetchGroup(ctx, name, sequence, options)
	if err != nil {
		return nil, err
	}
	return &GroupConsumer{inner: inner}, nil
}

// Resolve returns the broadcast serving a catalog rendition, honoring its broadcast
// reference: ffi.MoqVideo.Broadcast / ffi.MoqAudio.Broadcast. A nil or empty reference names this
// broadcast, anything else names a sibling relative to it (e.g. "./source"). Call it on a
// rendition that carries one before media.NewContainerConsumer, SubscribeTrack, FetchGroup, or
// media.NewContainerGroupConsumer, which take a track name rather than a rendition; DecodeAudio and
// DecodeVideo resolve it themselves.
//
// Errors if this broadcast came from a local producer rather than an origin, since a
// standalone broadcast has no sibling to name.
func (b *BroadcastConsumer) Resolve(ctx context.Context, reference *string) (*BroadcastConsumer, error) {
	inner, err := b.inner.Resolve(ctx, reference)
	if err != nil {
		return nil, err
	}
	return &BroadcastConsumer{inner: inner}, nil
}

// DecodeAudio subscribes to a raw-audio track; samples come back in the
// format declared by output. catalogAudio comes from the catalog.
func (b *BroadcastConsumer) DecodeAudio(
	ctx context.Context,
	name string,
	catalogAudio ffi.MoqAudio,
	output AudioDecoderOutput,
) (*AudioConsumer, error) {
	inner, err := b.inner.DecodeAudio(ctx, name, catalogAudio, output)
	if err != nil {
		return nil, err
	}
	return &AudioConsumer{inner: inner}, nil
}

// DecodeVideo subscribes to a video track and decodes it inside the bindings.
// catalogVideo comes from the catalog. output.Format picks the packed CPU layout
// every frame arrives in, defaulting to I420 when nil; each frame repeats it.
// output.Resize is best effort, so read each frame's own dimensions.
func (b *BroadcastConsumer) DecodeVideo(
	ctx context.Context,
	name string,
	catalogVideo ffi.MoqVideo,
	output VideoDecoderOutput,
) (*VideoConsumer, error) {
	inner, err := b.inner.DecodeVideo(ctx, name, catalogVideo, output)
	if err != nil {
		return nil, err
	}
	return &VideoConsumer{inner: inner}, nil
}

// GroupConsumer is a stream of timestamped raw frames within a single group.
type GroupConsumer struct {
	inner *ffi.MoqGroupConsumer
}

// Sequence is this group's sequence number within the track.
func (g *GroupConsumer) Sequence() uint64 {
	return g.inner.Sequence()
}

// ReadFrame returns the next timestamped frame, or (nil, nil) when the group ends.
func (g *GroupConsumer) ReadFrame(ctx context.Context) (*Frame, error) {
	frame, err := bridge.Call(ctx, g.inner.Cancel, g.inner.ReadFrame)
	return frameFromFFI(frame), err
}

// All ranges over timestamped frames until the group ends or the loop breaks.
func (g *GroupConsumer) All(ctx context.Context) iter.Seq2[*Frame, error] {
	return bridge.Seq(ctx, g.ReadFrame)
}

// Cancel stops the stream.
func (g *GroupConsumer) Cancel() {
	g.inner.Cancel()
}

// TrackConsumer is a stream of groups from a track. Each group is itself a
// stream of timestamped raw frames.
type TrackConsumer struct {
	inner *ffi.MoqTrackConsumer
}

// RecvGroup returns the next group in arrival order (possibly out of sequence),
// or (nil, nil) when the track ends. Prefer this for live, latency-sensitive
// consumption.
func (t *TrackConsumer) RecvGroup(ctx context.Context) (*GroupConsumer, error) {
	res, err := bridge.CallHandle(ctx, t.inner.Cancel, func(ctx context.Context) (*ffi.MoqGroupConsumer, error) {
		res, err := t.inner.RecvGroup(ctx)
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

// NextGroup returns the next group in sequence order, skipping forward if
// behind, or (nil, nil) when the track ends. Prefer this when order matters
// more than latency. Shares the sequence cursor with ReadFrame: a group one
// method has already taken is not returned by the other.
func (t *TrackConsumer) NextGroup(ctx context.Context) (*GroupConsumer, error) {
	res, err := bridge.CallHandle(ctx, t.inner.Cancel, func(ctx context.Context) (*ffi.MoqGroupConsumer, error) {
		res, err := t.inner.NextGroup(ctx)
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

// ReadFrame reads the first timestamped frame of the next group, or (nil, nil)
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
func (t *TrackConsumer) Info() (TrackInfo, error) {
	info, err := t.inner.Info()
	if err != nil {
		return TrackInfo{}, err
	}
	return trackInfoFromFFI(info), nil
}

// Update changes this subscriber's delivery preferences. It fails only on a
// negative MaxDelay.
func (t *TrackConsumer) Update(subscription Subscription) error {
	sub, err := subscription.ffi()
	if err != nil {
		return err
	}
	t.inner.Update(sub)
	return nil
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

// AudioConsumer is a stream of decoded audio frames.
type AudioConsumer struct {
	inner *ffi.MoqAudioConsumer
}

// Next returns the next audio frame, or (nil, nil) when the track ends.
func (a *AudioConsumer) Next(ctx context.Context) (*AudioFrame, error) {
	return bridge.Call(ctx, a.inner.Cancel, a.inner.Next)
}

// All ranges over audio frames until the track ends or the loop breaks.
func (a *AudioConsumer) All(ctx context.Context) iter.Seq2[*AudioFrame, error] {
	return bridge.Seq(ctx, a.Next)
}

// Cancel stops the stream.
func (a *AudioConsumer) Cancel() {
	a.inner.Cancel()
}

// VideoConsumer is a stream of decoded video frames.
type VideoConsumer struct {
	inner *ffi.MoqVideoConsumer
}

// Next returns the next decoded frame, or (nil, nil) when the track ends.
// Close each frame when done with it.
func (v *VideoConsumer) Next(ctx context.Context) (*VideoDecodedFrame, error) {
	res, err := bridge.CallHandle(ctx, v.inner.Cancel, func(ctx context.Context) (*ffi.MoqVideoDecodedFrame, error) {
		res, err := v.inner.Next(ctx)
		if err != nil || res == nil {
			return nil, err
		}
		return *res, nil
	})
	if err != nil || res == nil {
		return nil, err
	}
	return &VideoDecodedFrame{inner: res}, nil
}

// All ranges over decoded frames until the track ends or the loop breaks.
func (v *VideoConsumer) All(ctx context.Context) iter.Seq2[*VideoDecodedFrame, error] {
	return bridge.Seq(ctx, v.Next)
}

// Cancel stops the stream.
func (v *VideoConsumer) Cancel() {
	v.inner.Cancel()
}

// VideoDecodedFrame is one decoded video frame. It owns the decoder's surface
// until Close, which returns it to the decoder: holding many frames stalls
// decoding. It stays valid after its VideoConsumer is cancelled.
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
// memory. Only a DecodeVideo with VideoDecoderOutput.Surface set produces one,
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
