package moq

import (
	"context"
	"iter"

	ffi "github.com/moq-dev/moq-go-ffi/moq"
)

// BroadcastConsumer reads tracks from a broadcast.
type BroadcastConsumer struct {
	inner *ffi.MoqBroadcastConsumer
}

// SubscribeCatalog subscribes to the broadcast's catalog track.
func (b *BroadcastConsumer) SubscribeCatalog() (*CatalogConsumer, error) {
	inner, err := b.inner.SubscribeCatalog()
	if err != nil {
		return nil, err
	}
	return &CatalogConsumer{inner: inner}, nil
}

// SubscribeTrack subscribes to a track, receiving arbitrary byte payloads.
// subscription tunes delivery (priority, ordering, group range); pass nil for defaults.
func (b *BroadcastConsumer) SubscribeTrack(name string, subscription *Subscription) (*TrackConsumer, error) {
	inner, err := b.inner.SubscribeTrack(name, subscription)
	if err != nil {
		return nil, err
	}
	return &TrackConsumer{inner: inner}, nil
}

// FetchGroup fetches one complete group by track name and group sequence
// without holding a live subscription.
func (b *BroadcastConsumer) FetchGroup(name string, sequence uint64, options *FetchGroupOptions) (*GroupConsumer, error) {
	inner, err := b.inner.FetchGroup(name, sequence, options)
	if err != nil {
		return nil, err
	}
	return &GroupConsumer{inner: inner}, nil
}

// SubscribeMedia subscribes to a media track, decoded with the given container.
// maxLatencyMs bounds buffering before a stalled group is skipped. subscription
// tunes delivery (priority, ordering, group range); pass nil for defaults.
func (b *BroadcastConsumer) SubscribeMedia(name string, container Container, maxLatencyMs uint64, subscription *Subscription) (*MediaConsumer, error) {
	inner, err := b.inner.SubscribeMedia(name, container, maxLatencyMs, subscription)
	if err != nil {
		return nil, err
	}
	return &MediaConsumer{inner: inner}, nil
}

// SubscribeAudio subscribes to a raw-audio track; samples come back in the
// format declared by output. catalogAudio comes from the catalog.
func (b *BroadcastConsumer) SubscribeAudio(name string, catalogAudio Audio, output AudioDecoderOutput) (*AudioConsumer, error) {
	inner, err := b.inner.SubscribeAudio(name, catalogAudio, output)
	if err != nil {
		return nil, err
	}
	return &AudioConsumer{inner: inner}, nil
}

// Catalog subscribes and returns the first catalog. It reports ErrClosed if the
// catalog track ends before any catalog arrives.
func (b *BroadcastConsumer) Catalog(ctx context.Context) (*Catalog, error) {
	consumer, err := b.SubscribeCatalog()
	if err != nil {
		return nil, err
	}
	defer consumer.Cancel()
	catalog, err := consumer.Next(ctx)
	if err != nil {
		return nil, err
	}
	if catalog == nil {
		return nil, ErrClosed
	}
	return catalog, nil
}

// MediaConsumer is a stream of decoded media frames.
type MediaConsumer struct {
	inner *ffi.MoqMediaConsumer
}

// Next returns the next frame, or (nil, nil) when the track ends.
func (m *MediaConsumer) Next(ctx context.Context) (*Frame, error) {
	return runCancellable(ctx, m.inner.Cancel, m.inner.Next)
}

// Frames ranges over frames until the track ends or the loop breaks.
func (m *MediaConsumer) Frames(ctx context.Context) iter.Seq2[*Frame, error] {
	return streamSeq(ctx, m.Next)
}

// Cancel stops the stream.
func (m *MediaConsumer) Cancel() {
	m.inner.Cancel()
}

// GroupConsumer is a stream of byte payloads within a single group.
type GroupConsumer struct {
	inner *ffi.MoqGroupConsumer
}

// Sequence is this group's sequence number within the track.
func (g *GroupConsumer) Sequence() uint64 {
	return g.inner.Sequence()
}

// ReadFrame returns the next frame payload, or (nil, nil) when the group ends.
func (g *GroupConsumer) ReadFrame(ctx context.Context) ([]byte, error) {
	res, err := runCancellable(ctx, g.inner.Cancel, g.inner.ReadFrame)
	if err != nil {
		return nil, err
	}
	if res == nil {
		return nil, nil
	}
	return *res, nil
}

// Frames ranges over frame payloads until the group ends or the loop breaks.
func (g *GroupConsumer) Frames(ctx context.Context) iter.Seq2[[]byte, error] {
	return func(yield func([]byte, error) bool) {
		for {
			frame, err := g.ReadFrame(ctx)
			if err != nil {
				yield(nil, err)
				return
			}
			if frame == nil {
				return
			}
			if !yield(frame, nil) {
				return
			}
		}
	}
}

// Cancel stops the stream.
func (g *GroupConsumer) Cancel() {
	g.inner.Cancel()
}

// TrackConsumer is a stream of groups from a track. Each group is itself a
// stream of byte payloads.
type TrackConsumer struct {
	inner *ffi.MoqTrackConsumer
}

// RecvGroup returns the next group in arrival order (possibly out of sequence),
// or (nil, nil) when the track ends. Prefer this for live, latency-sensitive
// consumption.
func (t *TrackConsumer) RecvGroup(ctx context.Context) (*GroupConsumer, error) {
	res, err := runCancellable(ctx, t.inner.Cancel, t.inner.RecvGroup)
	if err != nil {
		return nil, err
	}
	if res == nil {
		return nil, nil
	}
	return &GroupConsumer{inner: *res}, nil
}

// NextGroup returns the next group in sequence order, skipping forward if
// behind, or (nil, nil) when the track ends. Prefer this when order matters
// more than latency.
func (t *TrackConsumer) NextGroup(ctx context.Context) (*GroupConsumer, error) {
	res, err := runCancellable(ctx, t.inner.Cancel, t.inner.NextGroup)
	if err != nil {
		return nil, err
	}
	if res == nil {
		return nil, nil
	}
	return &GroupConsumer{inner: *res}, nil
}

// ReadFrame reads the first frame of the next group, or (nil, nil) when the
// track ends. Convenient for one-frame-per-group tracks.
func (t *TrackConsumer) ReadFrame(ctx context.Context) ([]byte, error) {
	res, err := runCancellable(ctx, t.inner.Cancel, t.inner.ReadFrame)
	if err != nil {
		return nil, err
	}
	if res == nil {
		return nil, nil
	}
	return *res, nil
}

// Groups ranges over groups in sequence order.
func (t *TrackConsumer) Groups(ctx context.Context) iter.Seq2[*GroupConsumer, error] {
	return streamSeq(ctx, t.NextGroup)
}

// GroupsAsArrived ranges over groups in arrival order, including
// out-of-sequence deliveries.
func (t *TrackConsumer) GroupsAsArrived(ctx context.Context) iter.Seq2[*GroupConsumer, error] {
	return streamSeq(ctx, t.RecvGroup)
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
	return runCancellable(ctx, a.inner.Cancel, a.inner.Next)
}

// Frames ranges over audio frames until the track ends or the loop breaks.
func (a *AudioConsumer) Frames(ctx context.Context) iter.Seq2[*AudioFrame, error] {
	return streamSeq(ctx, a.Next)
}

// Cancel stops the stream.
func (a *AudioConsumer) Cancel() {
	a.inner.Cancel()
}

// CatalogConsumer is a stream of catalog updates.
type CatalogConsumer struct {
	inner *ffi.MoqCatalogConsumer
}

// Next returns the next catalog, or (nil, nil) when the track ends.
func (c *CatalogConsumer) Next(ctx context.Context) (*Catalog, error) {
	return runCancellable(ctx, c.inner.Cancel, c.inner.Next)
}

// Updates ranges over catalog updates until the track ends or the loop breaks.
func (c *CatalogConsumer) Updates(ctx context.Context) iter.Seq2[*Catalog, error] {
	return streamSeq(ctx, c.Next)
}

// Cancel stops the stream.
func (c *CatalogConsumer) Cancel() {
	c.inner.Cancel()
}
