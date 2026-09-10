package moq

import (
	"context"
	"errors"
	"iter"

	ffi "moq.dev/moq-ffi/moq"
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

// Dynamic advertises pattern and serves the requests beneath it.
//
// pattern is in the path Pattern dialect; a prefix is spelled "foo/**".
// Until wildcard advertisements land, anything but a prefix-shaped pattern
// is refused. Create, Dynamic if tracks are served on demand, populate, then
// [BroadcastProducer.Announce].
func (o *OriginProducer) Dynamic(pattern string, route Route) (*OriginDynamic, error) {
	inner, err := o.inner.Dynamic(pattern, route)
	if err != nil {
		return nil, err
	}
	return &OriginDynamic{inner: inner}, nil
}

// CreateBroadcast creates a broadcast at the given path, returning the producer
// that feeds it.
//
// The broadcast starts unadvertised: reachable by exact path, but not visible
// to announcement streams. Advertise it with [BroadcastProducer.Announce]
// after populating tracks. Finish unpublishes immediately, while dropping the
// producer without finishing also unpublishes but reads to subscribers as a
// failure rather than a deliberate end.
func (o *OriginProducer) CreateBroadcast(path string) (*BroadcastProducer, error) {
	inner, err := o.inner.CreateBroadcast(path)
	if err != nil {
		return nil, err
	}
	return &BroadcastProducer{inner: inner}, nil
}

// OriginDynamic streams requests for paths with no existing exact-path broadcast.
type OriginDynamic struct {
	inner *ffi.MoqOriginDynamic
}

// RequestedBroadcast blocks until a consumer requests a path with no existing
// exact-path broadcast.
func (d *OriginDynamic) RequestedBroadcast(ctx context.Context) (*BroadcastRequest, error) {
	inner, err := runHandle(ctx, d.inner.Cancel, d.inner.RequestedBroadcast)
	if err != nil {
		return nil, err
	}
	return &BroadcastRequest{inner: inner}, nil
}

// Requests ranges over requested broadcasts until the stream errors or the loop
// breaks.
func (d *OriginDynamic) Requests(ctx context.Context) iter.Seq2[*BroadcastRequest, error] {
	return streamSeq(ctx, d.RequestedBroadcast)
}

// Update re-prices the route in place: replaces its hops and cost.
func (d *OriginDynamic) Update(route Route) error {
	return d.inner.Update(route)
}

// Cancel stops serving requested broadcasts and retracts the route.
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

// Abort fails the request with an application error code.
func (r *BroadcastRequest) Abort(errorCode uint16) error {
	return r.inner.Abort(errorCode)
}

// OriginConsumer discovers and requests broadcasts published to an origin.
type OriginConsumer struct {
	inner *ffi.MoqOriginConsumer
}

// Announced streams route announcements whose prefix starts with prefix.
func (o *OriginConsumer) Announced(prefix string) (*Announced, error) {
	inner, err := o.inner.Announced(prefix)
	if err != nil {
		return nil, err
	}
	return &Announced{inner: inner}, nil
}

// AnnouncedBroadcast waits for a route covering an exact path, then resolves
// the broadcast there.
func (o *OriginConsumer) AnnouncedBroadcast(path string) (*AnnouncedBroadcast, error) {
	inner, err := o.inner.AnnouncedBroadcast(path)
	if err != nil {
		return nil, err
	}
	return &AnnouncedBroadcast{inner: inner}, nil
}

// RequestBroadcast resolves a broadcast at path as soon as it can be served: a
// local broadcast at the exact path, the best announced route covering it
// (served on demand by the session that announced it), or a dynamic fallback on
// the origin; errors if nothing can serve it. Unlike AnnouncedBroadcast, it
// does not wait for a future announcement. Blocks until resolved.
func (o *OriginConsumer) RequestBroadcast(ctx context.Context, path string) (*BroadcastConsumer, error) {
	inner, err := runOperation(ctx, func(cancel *ffi.MoqCancel) (*ffi.MoqBroadcastConsumer, error) {
		return o.inner.RequestBroadcast(path, &cancel)
	})
	if err != nil {
		return nil, err
	}
	return &BroadcastConsumer{inner: inner}, nil
}

// Announcement is a route announcement or retraction. A route claims that
// paths under Path can be served; it carries no broadcast. Resolve a specific
// path with [OriginConsumer.RequestBroadcast]. By convention a publisher
// announces each broadcast's exact path.
type Announcement struct {
	inner *ffi.MoqAnnouncement
}

// Path is the announced route's prefix, relative to the Announced prefix.
func (a *Announcement) Path() string {
	return a.inner.Path()
}

// Active reports whether the route is active (true) or was retracted (false).
// A repeated active announcement for the same path is a metadata update.
func (a *Announcement) Active() bool {
	return a.inner.Active()
}

// Route is the route serving the prefix: its relay hops and cost.
func (a *Announcement) Route() Route {
	return a.inner.Route()
}

// Announced is a stream of route announcements and retractions.
type Announced struct {
	inner *ffi.MoqAnnounced
}

// Next returns the next announcement, or (nil, nil) when the stream ends.
func (a *Announced) Next(ctx context.Context) (*Announcement, error) {
	res, err := runHandle(ctx, a.inner.Cancel, func() (*ffi.MoqAnnouncement, error) {
		res, err := a.inner.Next()
		if err != nil || res == nil {
			return nil, err
		}
		return *res, nil
	})
	if err != nil || res == nil {
		return nil, err
	}
	return &Announcement{inner: res}, nil
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
	inner, err := runHandle(ctx, a.inner.Cancel, a.inner.Available)
	if err != nil {
		return nil, err
	}
	return &BroadcastConsumer{inner: inner}, nil
}

// Cancel stops awaiting the broadcast.
func (a *AnnouncedBroadcast) Cancel() {
	a.inner.Cancel()
}
