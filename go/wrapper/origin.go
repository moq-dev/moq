package moq

import (
	"context"
	"iter"

	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// OriginProducer publishes broadcasts under paths and hands out consumers that
// discover them. Wire one as both a client's/server's publish source and
// consume sink for a full-duplex peer.
//
// There is no Close: the origin ends once the collector reaches every owner
// (each producer and [Route]), and its consumers
// then fail with [ErrClosed]. Keep an owner reachable for as long as the origin
// should live.
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

// Route creates a route at exactly path, owning a new broadcast to fill.
//
// The route is visible at once unless options.Hidden: a hidden route is
// invisible and unroutable, for this origin's consumers and peers alike, until
// [Route.Announce], so create it hidden to fill the catalog first. It serves a
// fresh instance, kept across [Route.Withdraw] and [Route.Announce], unless
// options names one. Keep the route reachable: [Route.Cancel], or the collector
// reclaiming it, retracts it, and [BroadcastProducer.Close] ends both.
func (o *OriginProducer) Route(path string, options RouteOptions) (*Route, error) {
	inner, err := o.inner.Route(path, options)
	if err != nil {
		return nil, err
	}
	return newRoute(inner), nil
}

// RoutePrefix creates a route over prefix that serves the requests beneath it.
//
// A route claims prefix and every path beneath it ("" claims every path). A
// service that only serves some of them rejects the rest as they are requested.
// Answer each [Route.Requested] request, and keep the route reachable while it
// should stay advertised.
func (o *OriginProducer) RoutePrefix(prefix string, options RouteOptions) (*Route, error) {
	inner, err := o.inner.RoutePrefix(prefix, options)
	if err != nil {
		return nil, err
	}
	return newRoute(inner), nil
}

// Route is a route on an origin: an exact route owning one broadcast, or a
// prefix route yielding the requests beneath it.
//
// [Route.Cancel], or the collector reclaiming it, retracts the route, as does
// closing an exact route's broadcast; a [BroadcastProducer] alone does not keep
// it announced.
type Route struct {
	inner     *ffi.MoqRoute
	broadcast *BroadcastProducer
}

func newRoute(inner *ffi.MoqRoute) *Route {
	route := &Route{inner: inner}
	// A prefix route owns no broadcast.
	if broadcast, err := inner.Broadcast(); err == nil {
		route.broadcast = &BroadcastProducer{inner: broadcast}
	}
	return route
}

// Broadcast returns the broadcast an exact route owns, the same one on every
// call, or an error on a prefix route, which owns none.
func (r *Route) Broadcast() (*BroadcastProducer, error) {
	if r.broadcast == nil {
		_, err := r.inner.Broadcast()
		return nil, err
	}
	return r.broadcast, nil
}

// Instance returns the stream this route serves, minted at creation unless
// [RouteOptions] named one.
func (r *Route) Instance() string {
	return r.inner.Instance()
}

// Announce makes the route visible, or keeps it so. A route from
// [BroadcastRequest.Route] also answers the requesters waiting on its path.
// Errors with ErrClosed once its broadcast is closed.
func (r *Route) Announce() error {
	return r.inner.Announce()
}

// Withdraw hides the route from local consumers and peers alike, keeping its
// broadcast and instance. Tracks already in flight carry on, and Announce
// brings the same stream back.
func (r *Route) Withdraw() error {
	return r.inner.Withdraw()
}

// SetCost re-prices the route in place: lower wins among routes covering the
// same prefix. A hidden route keeps the price for when it next announces.
func (r *Route) SetCost(cost uint64) error {
	return r.inner.SetCost(cost)
}

// Requested blocks until a consumer requests a path beneath this prefix route,
// returning (nil, nil) once the origin closes, and at once on an exact route.
//
// Cancelling ctx abandons only this wait: the route stays advertised.
func (r *Route) Requested(ctx context.Context) (*BroadcastRequest, error) {
	inner, err := bridge.CallRelease(ctx, nil, r.inner.Requested, func(request **ffi.MoqBroadcastRequest) {
		if request != nil {
			(*request).Destroy()
		}
	})
	if err != nil || inner == nil {
		return nil, err
	}
	return &BroadcastRequest{inner: *inner}, nil
}

// All ranges over requested broadcasts until the route ends, a call errors, or
// the loop breaks.
func (r *Route) All(ctx context.Context) iter.Seq2[*BroadcastRequest, error] {
	return bridge.Seq(ctx, r.Requested)
}

// Cancel retracts the route now, rather than when the collector reclaims it.
// Pending requests are rejected and later calls fail with ErrCancelled. An exact
// route's broadcast lives on until [BroadcastProducer.Close].
func (r *Route) Cancel() {
	r.inner.Cancel()
}

// BroadcastRequest is a requested broadcast that has not been answered yet.
type BroadcastRequest struct {
	inner *ffi.MoqBroadcastRequest
}

// Path returns the requested broadcast path.
func (r *BroadcastRequest) Path() (string, error) {
	return r.inner.Path()
}

// Route answers with the path's own exact route, owning a new broadcast.
//
// It serves a fresh instance unless options names one. Created visible, it
// announces now and answers the waiting requesters; created hidden, they wait
// until [Route.Announce], so the job can fill its catalog first.
func (r *BroadcastRequest) Route(options RouteOptions) (*Route, error) {
	inner, err := r.inner.Route(options)
	if err != nil {
		return nil, err
	}
	return newRoute(inner), nil
}

// Serve answers with held content under the prefix route's continuous sequence space,
// without announcing an exact route. The broadcast remains the caller's to release.
func (r *BroadcastRequest) Serve(broadcast *BroadcastConsumer) error {
	return r.inner.Serve(broadcast.inner)
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

// Follow follows an exact path, resolving each stream that serves it. A non-nil
// instance follows only that stream: another one taking over is never handed out,
// and the stream is handed out again if it returns after an end.
func (o *OriginConsumer) Follow(path string, instance *string) (*Following, error) {
	inner, err := o.inner.Follow(path, instance)
	if err != nil {
		return nil, err
	}
	return &Following{inner: inner}, nil
}

// Request resolves one publisher instance of the broadcast at path as soon as
// it can be served, through the best announced route covering it: an announced
// broadcast on this origin, a route a session announced (served on demand by
// that session), or a dynamic handler on the origin; errors if nothing can
// serve it, including a broadcast created but not announced. Unlike Follow, it
// does not wait for a future announcement. A non-nil instance refuses any
// other stream. Blocks until resolved.
func (o *OriginConsumer) Request(ctx context.Context, path string, instance *string) (*BroadcastConsumer, error) {
	inner, err := o.inner.Request(ctx, path, instance)
	if err != nil {
		return nil, err
	}
	return &BroadcastConsumer{inner: inner}, nil
}

// AnnounceConsumer is a stream of announce events.
type AnnounceConsumer struct {
	inner *ffi.MoqAnnounceConsumer
}

// Next returns the next announce event, or (nil, nil) when the stream ends.
func (a *AnnounceConsumer) Next(ctx context.Context) (AnnounceEvent, error) {
	res, err := bridge.Call(ctx, a.inner.Cancel, a.inner.Next)
	if err != nil || res == nil {
		return nil, err
	}
	return *res, nil
}

// All ranges over announce events until the stream ends or the loop breaks.
func (a *AnnounceConsumer) All(ctx context.Context) iter.Seq2[AnnounceEvent, error] {
	return bridge.Seq(ctx, a.Next)
}

// Cancel stops the announcement stream.
func (a *AnnounceConsumer) Cancel() {
	a.inner.Cancel()
}

// Following follows a specific broadcast across its publisher instances.
type Following struct {
	inner *ffi.MoqFollowing
}

// Available blocks until the next publisher instance serves the broadcast and
// returns its consumer. The first call resolves the instance serving the path once
// one is announced. Each later call waits for another to take over (a restarted
// publisher, or a replacement session) or for the broadcast to return after it went
// offline. Drop what was read from the previous instance then: its tracks end when
// its session does.
func (a *Following) Available(ctx context.Context) (*BroadcastConsumer, error) {
	inner, err := bridge.CallHandle(ctx, a.inner.Cancel, a.inner.Available)
	if err != nil {
		return nil, err
	}
	return &BroadcastConsumer{inner: inner}, nil
}

// Cancel stops awaiting the broadcast.
func (a *Following) Cancel() {
	a.inner.Cancel()
}
