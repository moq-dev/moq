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
	return NewOriginProducerWithConfig(OriginConfig{})
}

// NewOriginProducerWithConfig creates an origin with explicit config.
func NewOriginProducerWithConfig(config OriginConfig) *OriginProducer {
	return &OriginProducer{inner: ffi.NewMoqOriginProducer(config)}
}

// Consume returns a consumer that observes broadcasts published to this origin.
func (o *OriginProducer) Consume() *OriginConsumer {
	return &OriginConsumer{inner: o.inner.Consume()}
}

// Dynamic advertises prefix and serves the requests beneath it.
//
// A route claims prefix and every path beneath it ("" claims every path). A
// service that only serves some of them rejects the rest as they are requested.
// Create, Dynamic if tracks are served on demand, populate, then
// [BroadcastProducer.Announce].
func (o *OriginProducer) Dynamic(prefix string, route Route) (*OriginDynamic, error) {
	inner, err := o.inner.Dynamic(prefix, route)
	if err != nil {
		return nil, err
	}
	return &OriginDynamic{inner: inner}, nil
}

// CreateBroadcast creates a broadcast at the given path, returning the producer
// that feeds it.
//
// The broadcast is invisible and unroutable, for this origin's consumers and
// peers alike, until [BroadcastProducer.Announce]. Announce it after
// populating tracks. [BroadcastProducer.Close] ends it for good; dropping the
// last handle does the same.
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

// Update re-prices the route in place: replaces its hops and costs.
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

// Reject fails the request with an application error code.
func (r *BroadcastRequest) Reject(errorCode uint16) error {
	return r.inner.Reject(errorCode)
}

// OriginConsumer discovers and requests broadcasts published to an origin.
type OriginConsumer struct {
	inner *ffi.MoqOriginConsumer
}

// AnnounceOptions scopes an announcement stream.
type AnnounceOptions struct {
	// Prefix is a literal path root beneath the origin.
	Prefix string
	// Filter is a pattern relative to Prefix. Nil matches every path beneath it.
	Filter *string
	// Hidden also lists paths with a segment starting with "." below Prefix.
	Hidden bool
}

// Announced streams routes under a literal prefix matching an optional pattern filter.
func (o *OriginConsumer) Announced(options AnnounceOptions) (*AnnounceConsumer, error) {
	inner, err := o.inner.Announced(ffi.MoqAnnounceConfig{
		Prefix: options.Prefix,
		Filter: options.Filter,
		Hidden: options.Hidden,
	})
	if err != nil {
		return nil, err
	}
	return &AnnounceConsumer{inner: inner}, nil
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

// RequestBroadcast resolves a broadcast at path as soon as it can be served,
// through the best announced route covering it: an announced broadcast on this
// origin, a route a session announced (served on demand by that session), or a
// dynamic handler on the origin; errors if nothing can serve it, including a
// broadcast created but not announced. Unlike AnnouncedBroadcast, it
// does not wait for a future announcement. Blocks until resolved.
func (o *OriginConsumer) RequestBroadcast(ctx context.Context, path string) (*BroadcastConsumer, error) {
	inner, err := o.inner.RequestBroadcast(ctx, path)
	if err != nil {
		return nil, err
	}
	return &BroadcastConsumer{inner: inner}, nil
}

// AnnounceUpdate is a route announcement or retraction. A route claims that
// Prefix and every path beneath it can be served; it carries no broadcast. Resolve a specific
// path with [OriginConsumer.RequestBroadcast]. By convention a publisher
// announces each broadcast's exact path.
type AnnounceUpdate struct {
	inner *ffi.MoqAnnounceUpdate
}

// Prefix is the covered prefix, relative to the origin.
func (a *AnnounceUpdate) Prefix() string {
	return a.inner.Prefix()
}

// Captures reports what each filter wildcard matched. Nil means the route only
// overlaps the scope without pinning every wildcard.
func (a *AnnounceUpdate) Captures() []string {
	captures := a.inner.Captures()
	if captures == nil {
		return nil
	}
	result := make([]string, len(*captures))
	copy(result, *captures)
	return result
}

// Active reports whether the route is active (true) or was retracted (false).
// A repeated active announcement for the same prefix is a metadata update.
func (a *AnnounceUpdate) Active() bool {
	return a.inner.Active()
}

// Route is the route serving the prefix: its relay hops and costs (warm Cost, undiscounted Cold).
func (a *AnnounceUpdate) Route() Route {
	return a.inner.Route()
}

// AnnounceConsumer is a stream of route announcements and retractions.
type AnnounceConsumer struct {
	inner *ffi.MoqAnnounceConsumer
}

// Next returns the next announcement, or (nil, nil) when the stream ends.
func (a *AnnounceConsumer) Next(ctx context.Context) (*AnnounceUpdate, error) {
	res, err := runHandle(ctx, a.inner.Cancel, func(ctx context.Context) (*ffi.MoqAnnounceUpdate, error) {
		res, err := a.inner.Next(ctx)
		if err != nil || res == nil {
			return nil, err
		}
		return *res, nil
	})
	if err != nil || res == nil {
		return nil, err
	}
	return &AnnounceUpdate{inner: res}, nil
}

// All ranges over announcements until the stream ends or the loop breaks.
func (a *AnnounceConsumer) All(ctx context.Context) iter.Seq2[*AnnounceUpdate, error] {
	return streamSeq(ctx, a.Next)
}

// Cancel stops the announcement stream.
func (a *AnnounceConsumer) Cancel() {
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
