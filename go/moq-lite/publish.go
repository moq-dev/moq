package moq

import (
	"context"

	moqffi "github.com/moq-dev/moq/go/moq-ffi/moq"
)

// MediaProducer publishes media frames on a media track.
type MediaProducer struct {
	inner *moqffi.MoqMediaProducer
}

func newMediaProducer(inner *moqffi.MoqMediaProducer) *MediaProducer {
	return &MediaProducer{inner: inner}
}

// Name returns the generated media track name.
func (m *MediaProducer) Name() (string, error) { return m.inner.Name() }

// Used blocks until this track has at least one active subscriber.
func (m *MediaProducer) Used(ctx context.Context) error {
	_, err := callCtx(ctx, func() {}, func() (struct{}, error) {
		return struct{}{}, m.inner.Used()
	})
	return err
}

// Unused blocks until this track has no active subscribers.
func (m *MediaProducer) Unused(ctx context.Context) error {
	_, err := callCtx(ctx, func() {}, func() (struct{}, error) {
		return struct{}{}, m.inner.Unused()
	})
	return err
}

func (m *MediaProducer) WriteFrame(payload []byte, timestampUs uint64) error {
	return m.inner.WriteFrame(payload, timestampUs)
}

func (m *MediaProducer) Finish() error { return m.inner.Finish() }
func (m *MediaProducer) Close()        { m.inner.Destroy() }

// GroupProducer writes frames into a single group on a track.
type GroupProducer struct {
	inner *moqffi.MoqGroupProducer
}

func newGroupProducer(inner *moqffi.MoqGroupProducer) *GroupProducer {
	return &GroupProducer{inner: inner}
}

func (g *GroupProducer) Sequence() uint64          { return g.inner.Sequence() }
func (g *GroupProducer) WriteFrame(p []byte) error { return g.inner.WriteFrame(p) }
func (g *GroupProducer) Finish() error             { return g.inner.Finish() }
func (g *GroupProducer) Close()                    { g.inner.Destroy() }

// Consume returns a consumer that reads frames from this group.
func (g *GroupProducer) Consume() (*GroupConsumer, error) {
	c, err := g.inner.Consume()
	if err != nil {
		return nil, err
	}
	return newGroupConsumer(c), nil
}

// TrackProducer publishes arbitrary byte frames on a non-media track.
//
// Same pattern as moq-boy's status/command tracks.
type TrackProducer struct {
	inner *moqffi.MoqTrackProducer
}

func newTrackProducer(inner *moqffi.MoqTrackProducer) *TrackProducer {
	return &TrackProducer{inner: inner}
}

func (t *TrackProducer) Name() (string, error) { return t.inner.Name() }

// Used blocks until this track has at least one active subscriber.
func (t *TrackProducer) Used(ctx context.Context) error {
	_, err := callCtx(ctx, func() {}, func() (struct{}, error) {
		return struct{}{}, t.inner.Used()
	})
	return err
}

// Unused blocks until this track has no active subscribers.
func (t *TrackProducer) Unused(ctx context.Context) error {
	_, err := callCtx(ctx, func() {}, func() (struct{}, error) {
		return struct{}{}, t.inner.Unused()
	})
	return err
}

// AppendGroup opens a new group; write frames into it, then Finish.
func (t *TrackProducer) AppendGroup() (*GroupProducer, error) {
	g, err := t.inner.AppendGroup()
	if err != nil {
		return nil, err
	}
	return newGroupProducer(g), nil
}

// WriteFrame writes a single-frame group in one call.
func (t *TrackProducer) WriteFrame(p []byte) error { return t.inner.WriteFrame(p) }

// Consume returns a consumer that reads from this producer's track directly,
// without going through an origin/broadcast.
func (t *TrackProducer) Consume() (*TrackConsumer, error) {
	c, err := t.inner.Consume()
	if err != nil {
		return nil, err
	}
	return newTrackConsumer(c), nil
}

func (t *TrackProducer) Finish() error { return t.inner.Finish() }
func (t *TrackProducer) Close()        { t.inner.Destroy() }

// BroadcastProducer publishes tracks within a broadcast.
type BroadcastProducer struct {
	inner *moqffi.MoqBroadcastProducer
}

// NewBroadcastProducer creates a broadcast for publishing media and/or
// raw tracks. The broadcast does nothing until published to an origin.
func NewBroadcastProducer() (*BroadcastProducer, error) {
	b, err := moqffi.NewMoqBroadcastProducer()
	if err != nil {
		return nil, err
	}
	return &BroadcastProducer{inner: b}, nil
}

// PublishMedia opens a new media track. `format` is the codec identifier
// (e.g. "opus", "avc3"), `init` is the codec-specific init payload.
func (b *BroadcastProducer) PublishMedia(format string, init []byte) (*MediaProducer, error) {
	m, err := b.inner.PublishMedia(format, init)
	if err != nil {
		return nil, err
	}
	return newMediaProducer(m), nil
}

// PublishTrack opens a new raw byte-payload track, no codec validation.
func (b *BroadcastProducer) PublishTrack(name string) (*TrackProducer, error) {
	t, err := b.inner.PublishTrack(name)
	if err != nil {
		return nil, err
	}
	return newTrackProducer(t), nil
}

// Consume returns a consumer that reads from this broadcast's tracks
// directly, without going through an origin.
func (b *BroadcastProducer) Consume() (*BroadcastConsumer, error) {
	c, err := b.inner.Consume()
	if err != nil {
		return nil, err
	}
	return newBroadcastConsumer(c), nil
}

func (b *BroadcastProducer) Finish() error { return b.inner.Finish() }
func (b *BroadcastProducer) Close()        { b.inner.Destroy() }
