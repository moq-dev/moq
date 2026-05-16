package moq

import (
	"context"
	"iter"

	moqffi "github.com/moq-dev/moq/go/moq-ffi/moq"
)

// Announcement is a discovered broadcast from an origin.
type Announcement struct {
	inner *moqffi.MoqAnnouncement
}

func (a *Announcement) Path() string { return a.inner.Path() }
func (a *Announcement) Broadcast() *BroadcastConsumer {
	return newBroadcastConsumer(a.inner.Broadcast())
}
func (a *Announcement) Close() { a.inner.Destroy() }

// Announced iterates broadcast announcements under a prefix.
type Announced struct {
	inner *moqffi.MoqAnnounced
}

func (a *Announced) Next(ctx context.Context) (*Announcement, error) {
	ann, err := callCtx(ctx, a.inner.Cancel, a.inner.Next)
	if err != nil || ann == nil {
		return nil, err
	}
	return &Announcement{inner: *ann}, nil
}

// Announcements iterates until the origin closes or ctx cancels.
func (a *Announced) Announcements(ctx context.Context) iter.Seq2[*Announcement, error] {
	return func(yield func(*Announcement, error) bool) {
		for {
			ann, err := a.Next(ctx)
			if err != nil {
				yield(nil, err)
				return
			}
			if ann == nil {
				return
			}
			if !yield(ann, nil) {
				ann.Close()
				return
			}
		}
	}
}

func (a *Announced) Cancel() { a.inner.Cancel() }
func (a *Announced) Close() {
	a.inner.Cancel()
	a.inner.Destroy()
}

// AnnouncedBroadcast is an awaitable for a specific broadcast path.
type AnnouncedBroadcast struct {
	inner *moqffi.MoqAnnouncedBroadcast
}

// Available blocks until the broadcast is announced, or ctx is cancelled.
func (a *AnnouncedBroadcast) Available(ctx context.Context) (*BroadcastConsumer, error) {
	b, err := callCtx(ctx, a.inner.Cancel, a.inner.Available)
	if err != nil {
		return nil, err
	}
	return newBroadcastConsumer(b), nil
}

func (a *AnnouncedBroadcast) Cancel() { a.inner.Cancel() }
func (a *AnnouncedBroadcast) Close() {
	a.inner.Cancel()
	a.inner.Destroy()
}

// OriginConsumer discovers broadcasts.
type OriginConsumer struct {
	inner *moqffi.MoqOriginConsumer
}

func (c *OriginConsumer) Announced(prefix string) (*Announced, error) {
	a, err := c.inner.Announced(prefix)
	if err != nil {
		return nil, err
	}
	return &Announced{inner: a}, nil
}

func (c *OriginConsumer) AnnouncedBroadcast(path string) (*AnnouncedBroadcast, error) {
	a, err := c.inner.AnnouncedBroadcast(path)
	if err != nil {
		return nil, err
	}
	return &AnnouncedBroadcast{inner: a}, nil
}

func (c *OriginConsumer) Close() { c.inner.Destroy() }

// OriginProducer publishes broadcasts.
type OriginProducer struct {
	inner *moqffi.MoqOriginProducer
}

func NewOriginProducer() *OriginProducer {
	return &OriginProducer{inner: moqffi.NewMoqOriginProducer()}
}

func (p *OriginProducer) Consume() *OriginConsumer {
	return &OriginConsumer{inner: p.inner.Consume()}
}

func (p *OriginProducer) Publish(path string, broadcast *BroadcastProducer) error {
	return p.inner.Publish(path, broadcast.inner)
}

func (p *OriginProducer) Close() { p.inner.Destroy() }
