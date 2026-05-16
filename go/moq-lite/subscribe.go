package moq

import (
	"context"
	"iter"

	moqffi "github.com/moq-dev/moq/go/moq-ffi/moq"
)

// MediaConsumer reads decoded media frames from a track.
//
// Iterate via Frames(ctx) for range-over-func consumption.
type MediaConsumer struct {
	inner *moqffi.MoqMediaConsumer
}

func newMediaConsumer(inner *moqffi.MoqMediaConsumer) *MediaConsumer {
	return &MediaConsumer{inner: inner}
}

// Next returns the next frame, or (nil, nil) at end of stream.
func (c *MediaConsumer) Next(ctx context.Context) (*Frame, error) {
	return callCtx(ctx, c.inner.Cancel, c.inner.Next)
}

// Frames returns an iterator over frames until the stream ends or ctx is
// cancelled. Errors abort iteration.
func (c *MediaConsumer) Frames(ctx context.Context) iter.Seq2[*Frame, error] {
	return func(yield func(*Frame, error) bool) {
		for {
			frame, err := c.Next(ctx)
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

// Cancel stops any pending Next call and any future calls.
func (c *MediaConsumer) Cancel() { c.inner.Cancel() }

// Close releases the underlying handle. Safe to call multiple times.
func (c *MediaConsumer) Close() {
	c.inner.Cancel()
	c.inner.Destroy()
}

// GroupConsumer reads frames from a single group on a track.
type GroupConsumer struct {
	inner *moqffi.MoqGroupConsumer
}

func newGroupConsumer(inner *moqffi.MoqGroupConsumer) *GroupConsumer {
	return &GroupConsumer{inner: inner}
}

// Sequence is this group's sequence number within the track.
func (g *GroupConsumer) Sequence() uint64 { return g.inner.Sequence() }

// ReadFrame returns the next frame in this group, or (nil, nil) at end.
func (g *GroupConsumer) ReadFrame(ctx context.Context) ([]byte, error) {
	frame, err := callCtx(ctx, g.inner.Cancel, g.inner.ReadFrame)
	if err != nil || frame == nil {
		return nil, err
	}
	return *frame, nil
}

// Frames iterates frame payloads until the group ends or ctx is cancelled.
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

func (g *GroupConsumer) Cancel() { g.inner.Cancel() }
func (g *GroupConsumer) Close() {
	g.inner.Cancel()
	g.inner.Destroy()
}

// TrackConsumer reads groups (or individual frames) from a track.
type TrackConsumer struct {
	inner *moqffi.MoqTrackConsumer
}

func newTrackConsumer(inner *moqffi.MoqTrackConsumer) *TrackConsumer {
	return &TrackConsumer{inner: inner}
}

// RecvGroup returns the next group in arrival order, or (nil, nil) at end.
//
// Groups may arrive out of sequence order. Use for live consumption where
// latency matters more than ordering.
func (t *TrackConsumer) RecvGroup(ctx context.Context) (*GroupConsumer, error) {
	group, err := callCtx(ctx, t.inner.Cancel, t.inner.RecvGroup)
	if err != nil || group == nil {
		return nil, err
	}
	return newGroupConsumer(*group), nil
}

// NextGroup returns the next group in sequence order, skipping forward when
// behind. Returns (nil, nil) at end. Prefer RecvGroup for live consumption.
func (t *TrackConsumer) NextGroup(ctx context.Context) (*GroupConsumer, error) {
	group, err := callCtx(ctx, t.inner.Cancel, t.inner.NextGroup)
	if err != nil || group == nil {
		return nil, err
	}
	return newGroupConsumer(*group), nil
}

// ReadFrame returns the first frame of the next group, or (nil, nil) at end.
// Convenience for one-frame-per-group tracks (like moq-boy's status track).
func (t *TrackConsumer) ReadFrame(ctx context.Context) ([]byte, error) {
	frame, err := callCtx(ctx, t.inner.Cancel, t.inner.ReadFrame)
	if err != nil || frame == nil {
		return nil, err
	}
	return *frame, nil
}

// Groups iterates groups in arrival order. The returned GroupConsumer must
// be drained or Close()d before requesting the next.
func (t *TrackConsumer) Groups(ctx context.Context) iter.Seq2[*GroupConsumer, error] {
	return func(yield func(*GroupConsumer, error) bool) {
		for {
			g, err := t.RecvGroup(ctx)
			if err != nil {
				yield(nil, err)
				return
			}
			if g == nil {
				return
			}
			if !yield(g, nil) {
				g.Close()
				return
			}
		}
	}
}

func (t *TrackConsumer) Cancel() { t.inner.Cancel() }
func (t *TrackConsumer) Close() {
	t.inner.Cancel()
	t.inner.Destroy()
}

// CatalogConsumer reads catalog updates from a broadcast.
type CatalogConsumer struct {
	inner *moqffi.MoqCatalogConsumer
}

func newCatalogConsumer(inner *moqffi.MoqCatalogConsumer) *CatalogConsumer {
	return &CatalogConsumer{inner: inner}
}

// Next returns the next catalog update, or (nil, nil) at end of stream.
func (c *CatalogConsumer) Next(ctx context.Context) (*Catalog, error) {
	return callCtx(ctx, c.inner.Cancel, c.inner.Next)
}

// Catalogs iterates catalog updates until the stream ends or ctx is cancelled.
func (c *CatalogConsumer) Catalogs(ctx context.Context) iter.Seq2[*Catalog, error] {
	return func(yield func(*Catalog, error) bool) {
		for {
			cat, err := c.Next(ctx)
			if err != nil {
				yield(nil, err)
				return
			}
			if cat == nil {
				return
			}
			if !yield(cat, nil) {
				return
			}
		}
	}
}

func (c *CatalogConsumer) Cancel() { c.inner.Cancel() }
func (c *CatalogConsumer) Close() {
	c.inner.Cancel()
	c.inner.Destroy()
}

// BroadcastConsumer subscribes to tracks and catalog updates within a
// broadcast.
type BroadcastConsumer struct {
	inner *moqffi.MoqBroadcastConsumer
}

func newBroadcastConsumer(inner *moqffi.MoqBroadcastConsumer) *BroadcastConsumer {
	return &BroadcastConsumer{inner: inner}
}

func (b *BroadcastConsumer) SubscribeCatalog() (*CatalogConsumer, error) {
	c, err := b.inner.SubscribeCatalog()
	if err != nil {
		return nil, err
	}
	return newCatalogConsumer(c), nil
}

func (b *BroadcastConsumer) SubscribeTrack(name string) (*TrackConsumer, error) {
	t, err := b.inner.SubscribeTrack(name)
	if err != nil {
		return nil, err
	}
	return newTrackConsumer(t), nil
}

func (b *BroadcastConsumer) SubscribeMedia(name string, container Container, maxLatencyMs uint64) (*MediaConsumer, error) {
	m, err := b.inner.SubscribeMedia(name, container, maxLatencyMs)
	if err != nil {
		return nil, err
	}
	return newMediaConsumer(m), nil
}

// Catalog subscribes to the catalog and returns the first update.
func (b *BroadcastConsumer) Catalog(ctx context.Context) (*Catalog, error) {
	c, err := b.SubscribeCatalog()
	if err != nil {
		return nil, err
	}
	defer c.Close()
	return c.Next(ctx)
}

func (b *BroadcastConsumer) Close() { b.inner.Destroy() }
