package moq

import (
	"context"
	"errors"
	"iter"

	ffi "github.com/moq-dev/moq-go-ffi/moq"
)

// OriginProducer publishes broadcasts under paths and hands out consumers that
// discover them. Wire one as both a client's/server's publish source and
// consume sink for a full-duplex peer.
type OriginProducer struct {
	inner *ffi.MoqOriginProducer
}

// NewOriginProducer creates an empty origin.
func NewOriginProducer() *OriginProducer {
	return NewOriginProducerWithOptions(OriginOptions{})
}

// NewOriginProducerWithOptions creates an origin with explicit options.
func NewOriginProducerWithOptions(options OriginOptions) *OriginProducer {
	return &OriginProducer{inner: ffi.NewMoqOriginProducer(options)}
}

// Consume returns a consumer that observes broadcasts published to this origin.
func (o *OriginProducer) Consume() *OriginConsumer {
	return &OriginConsumer{inner: o.inner.Consume()}
}

// Dynamic serves broadcasts that consumers request without an announcement.
func (o *OriginProducer) Dynamic() *OriginDynamic {
	return &OriginDynamic{inner: o.inner.Dynamic()}
}

// Announce advertises a broadcast at the given path so subscribers can discover it.
func (o *OriginProducer) Announce(path string, broadcast *BroadcastProducer) error {
	if broadcast == nil {
		return errors.New("moq: nil broadcast producer")
	}
	return o.inner.Announce(path, broadcast.inner)
}

// Deprecated: use Announce.
func (o *OriginProducer) Publish(path string, broadcast *BroadcastProducer) error {
	return o.Announce(path, broadcast)
}

// OriginDynamic streams broadcast requests for paths that are not announced.
type OriginDynamic struct {
	inner *ffi.MoqOriginDynamic
}

// RequestedBroadcast blocks until a consumer requests an unannounced broadcast.
func (d *OriginDynamic) RequestedBroadcast(ctx context.Context) (*BroadcastRequest, error) {
	inner, err := runCancellable(ctx, d.inner.Cancel, d.inner.RequestedBroadcast)
	if err != nil {
		return nil, err
	}
	return &BroadcastRequest{inner: inner}, nil
}

// All ranges over requested broadcasts until the stream errors or the loop breaks.
func (d *OriginDynamic) All(ctx context.Context) iter.Seq2[*BroadcastRequest, error] {
	return streamSeq(ctx, d.RequestedBroadcast)
}

// Cancel stops serving requested broadcasts.
func (d *OriginDynamic) Cancel() {
	d.inner.Cancel()
}

// BroadcastRequest is a requested broadcast that has not been accepted yet.
type BroadcastRequest struct {
	inner *ffi.MoqBroadcastRequest
}

// Path returns the requested broadcast path.
func (r *BroadcastRequest) Path() (string, error) {
	return r.inner.Path()
}

// Accept serves the request with an unannounced broadcast.
func (r *BroadcastRequest) Accept(broadcast *BroadcastProducer) error {
	if broadcast == nil {
		return errors.New("moq: nil broadcast producer")
	}
	return r.inner.Accept(broadcast.inner)
}

// Reject fails the request with an application error code.
func (r *BroadcastRequest) Reject(errorCode int32) error {
	return r.inner.Reject(errorCode)
}

// OriginConsumer discovers broadcasts announced to an origin.
type OriginConsumer struct {
	inner *ffi.MoqOriginConsumer
}

// Announced streams broadcasts whose path starts with prefix.
func (o *OriginConsumer) Announced(prefix string) (*Announced, error) {
	inner, err := o.inner.Announced(prefix)
	if err != nil {
		return nil, err
	}
	return &Announced{inner: inner}, nil
}

// AnnouncedBroadcast resolves a single broadcast at an exact path.
func (o *OriginConsumer) AnnouncedBroadcast(path string) (*AnnouncedBroadcast, error) {
	inner, err := o.inner.AnnouncedBroadcast(path)
	if err != nil {
		return nil, err
	}
	return &AnnouncedBroadcast{inner: inner}, nil
}

// RequestBroadcast resolves a broadcast at path as soon as it can be served: the
// announced broadcast if present, otherwise a dynamic fallback on the origin, or an
// error if neither can serve it. Unlike AnnouncedBroadcast, it does not wait for a
// future announcement. Blocks until resolved.
func (o *OriginConsumer) RequestBroadcast(path string) (*BroadcastConsumer, error) {
	inner, err := o.inner.RequestBroadcast(path)
	if err != nil {
		return nil, err
	}
	return &BroadcastConsumer{inner: inner}, nil
}

// Announcement is a discovered broadcast.
type Announcement struct {
	inner *ffi.MoqAnnouncement
}

// Path is the broadcast's announced path.
func (a *Announcement) Path() string {
	return a.inner.Path()
}

// Broadcast returns a consumer for the announced broadcast's tracks.
func (a *Announcement) Broadcast() *BroadcastConsumer {
	return &BroadcastConsumer{inner: a.inner.Broadcast()}
}

// Announced is a stream of broadcast announcements.
type Announced struct {
	inner *ffi.MoqAnnounced
}

// Next returns the next announcement, or (nil, nil) when the stream ends.
func (a *Announced) Next(ctx context.Context) (*Announcement, error) {
	res, err := runCancellable(ctx, a.inner.Cancel, a.inner.Next)
	if err != nil {
		return nil, err
	}
	if res == nil {
		return nil, nil
	}
	return &Announcement{inner: *res}, nil
}

// All ranges over announcements until the stream ends or the loop breaks.
func (a *Announced) All(ctx context.Context) iter.Seq2[*Announcement, error] {
	return streamSeq(ctx, a.Next)
}

// Cancel stops the announcement stream.
func (a *Announced) Cancel() {
	a.inner.Cancel()
}

// AnnouncedBroadcast awaits a specific broadcast becoming available.
type AnnouncedBroadcast struct {
	inner *ffi.MoqAnnouncedBroadcast
}

// Available blocks until the broadcast is available and returns its consumer.
func (a *AnnouncedBroadcast) Available(ctx context.Context) (*BroadcastConsumer, error) {
	inner, err := runCancellable(ctx, a.inner.Cancel, a.inner.Available)
	if err != nil {
		return nil, err
	}
	return &BroadcastConsumer{inner: inner}, nil
}

// Cancel stops awaiting the broadcast.
func (a *AnnouncedBroadcast) Cancel() {
	a.inner.Cancel()
}
