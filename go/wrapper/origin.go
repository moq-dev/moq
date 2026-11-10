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
// (each producer, [Route], and [Claim]), and its consumers
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

// RouteOptions is how a [Route] or [Claim] advertises itself. The zero value is visible at
// once, at cost 0, under a fresh instance the route keeps for its lifetime, so a restarted
// publisher is a new stream viewers restart onto.
type RouteOptions struct {
	// Hidden creates the route invisible and unroutable until [Route.Announce].
	Hidden bool
	// Cost is the static production cost: lower wins among routes covering the same prefix.
	Cost uint64
	// Instance is a stable stream to serve, or nil to mint a fresh one. Set it only for a
	// publisher that continues its group sequences across restarts and raises Epoch on each,
	// or to forward an announced route's stream as is: readers stitch every route of one
	// instance together, so restarting the sequences under an old instance splices unrelated
	// groups into their tracks.
	Instance *Instance
	// Epoch is which run of a stable Instance this is: only the highest is eligible. Ignored without one.
	Epoch uint64
}

func (o RouteOptions) ffi() ffi.MoqRouteOptions {
	return ffi.MoqRouteOptions{Hidden: o.Hidden, Cost: o.Cost, Instance: optional(o.Instance), Epoch: o.Epoch}
}

// optional lowers a nil instance to the ffi's None.
func optional(instance *Instance) **ffi.MoqInstance {
	if instance == nil {
		return nil
	}
	return &instance
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
	inner, err := o.inner.Route(path, options.ffi())
	if err != nil {
		return nil, err
	}
	return newRoute(inner), nil
}

// Claim claims prefix and serves the requests beneath it.
//
// A claim covers prefix and every path beneath it ("" claims every path). A
// service that only serves some of them rejects the rest as they are requested.
// Answer each [Claim.Requested] request, and keep the claim reachable while it
// should stay advertised.
func (o *OriginProducer) Claim(prefix string, options RouteOptions) (*Claim, error) {
	inner, err := o.inner.Claim(prefix, options.ffi())
	if err != nil {
		return nil, err
	}
	return &Claim{inner: inner}, nil
}

// Route is an exact route on an origin, owning one broadcast.
//
// [Route.Cancel], or the collector reclaiming it, retracts the route, as does
// closing its broadcast; a [BroadcastProducer] alone does not keep it
// announced.
type Route struct {
	inner     *ffi.MoqRoute
	broadcast *BroadcastProducer
}

func newRoute(inner *ffi.MoqRoute) *Route {
	return &Route{inner: inner, broadcast: &BroadcastProducer{inner: inner.Broadcast()}}
}

// Broadcast returns the broadcast the route owns, the same one on every call.
func (r *Route) Broadcast() *BroadcastProducer {
	return r.broadcast
}

// Instance returns the stream this route serves now. A named instance never
// changes. A job's route takes its claim's current one when it first announces
// or when this first reads it. Read first, the job announces under it, and
// [Route.Announce] fails with Unroutable once the claim has moved past it, so a
// signer built from it stays valid. Once cancelled, it is the last one read or
// announced, or nil for a hidden route that did neither.
func (r *Route) Instance() *Instance {
	if instance := r.inner.InstanceId(); instance != nil {
		return *instance
	}
	return nil
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

// Cancel retracts the route now, rather than when the collector reclaims it.
// Later calls fail with ErrCancelled. Its broadcast lives on until
// [BroadcastProducer.Close].
func (r *Route) Cancel() {
	r.inner.Cancel()
}

// Claim is a prefix route on an origin: a claim that every path beneath a
// prefix can be served, yielding each request as it arrives.
//
// [Claim.Cancel], or the collector reclaiming it, retracts the claim and
// rejects the requests still waiting.
type Claim struct {
	inner *ffi.MoqClaim
}

// Instance returns the stream this claim serves now. A named instance never
// changes. Unnamed, the claim moves to a fresh one each time a job holding its
// current one ends.
func (c *Claim) Instance() *Instance {
	return c.inner.InstanceId()
}

// Announce makes the claim visible, or keeps it so.
func (c *Claim) Announce() error {
	return c.inner.Announce()
}

// Withdraw hides the claim from local consumers and peers alike, keeping its
// instance: Announce brings it back. Requests beneath it are rejected
// meanwhile.
func (c *Claim) Withdraw() error {
	return c.inner.Withdraw()
}

// SetCost re-prices the claim in place: lower wins among routes covering the
// same prefix. A hidden claim keeps the price for when it next announces.
func (c *Claim) SetCost(cost uint64) error {
	return c.inner.SetCost(cost)
}

// Requested blocks until a consumer requests a path beneath this claim,
// returning (nil, nil) once the claim is retracted or the origin closes.
//
// Cancelling ctx abandons only this wait: the claim stays advertised.
func (c *Claim) Requested(ctx context.Context) (*BroadcastRequest, error) {
	inner, err := bridge.CallRelease(ctx, nil, c.inner.Requested, func(request **ffi.MoqBroadcastRequest) {
		if request != nil {
			(*request).Destroy()
		}
	})
	if err != nil || inner == nil {
		return nil, err
	}
	return &BroadcastRequest{inner: *inner}, nil
}

// All ranges over requested broadcasts until the claim ends, a call errors, or
// the loop breaks. Ending ctx stops the loop but leaves the claim advertised.
func (c *Claim) All(ctx context.Context) iter.Seq2[*BroadcastRequest, error] {
	return bridge.Seq(ctx, nil, c.Requested)
}

// Cancel retracts the claim now, rather than when the collector reclaims it.
// Pending requests are rejected and later calls fail with ErrCancelled.
func (c *Claim) Cancel() {
	c.inner.Cancel()
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
// Unless options names an instance, it takes the one its claim lends when it
// first announces or [Route.Instance] first reads it, so readers that resolved
// the path through the claim continue on it; its end then moves the claim to a
// fresh instance, so a restarted job is a new stream. A claim that names its
// instance lends none, and the route mints its own. Created visible, it announces now and answers
// the waiting requesters; created hidden, they wait
// until [Route.Announce], so the job can fill its catalog first.
func (r *BroadcastRequest) Route(options RouteOptions) (*Route, error) {
	inner, err := r.inner.Route(options.ffi())
	if err != nil {
		return nil, err
	}
	return newRoute(inner), nil
}

// Serve answers with held content under the claim's continuous sequence space,
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

// Announced lists the paths under a literal prefix matching an optional pattern filter, as each
// becomes present, absent, or offline, or changes availability. To see another publisher instance take a path over, use Follow.
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

// Follow follows an exact path, resolving each stream that serves it and reporting
// when it goes offline. A non-nil instance follows only that stream: another one
// taking over reads as offline, and the stream is handed out again if it returns.
func (o *OriginConsumer) Follow(path string, instance *Instance) (*Following, error) {
	inner, err := o.inner.Follow(path, optional(instance))
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
func (o *OriginConsumer) Request(ctx context.Context, path string, instance *Instance) (*BroadcastConsumer, error) {
	inner, err := o.inner.Request(ctx, path, optional(instance))
	if err != nil {
		return nil, err
	}
	return &BroadcastConsumer{inner: inner}, nil
}

// AnnounceConsumer is a listing of paths, each present, absent, or offline.
type AnnounceConsumer struct {
	inner *ffi.MoqAnnounceConsumer
}

// Next returns the next change to a path, or (nil, nil) when the stream ends.
func (a *AnnounceConsumer) Next(ctx context.Context) (AnnounceEvent, error) {
	res, err := bridge.Call(ctx, nil, a.inner.Next)
	if err != nil || res == nil {
		return nil, err
	}
	return *res, nil
}

// All ranges over the listing's events until the stream ends or the loop breaks.
func (a *AnnounceConsumer) All(ctx context.Context) iter.Seq2[AnnounceEvent, error] {
	return bridge.Seq(ctx, a.inner.Cancel, a.Next)
}

// Cancel stops the listing.
func (a *AnnounceConsumer) Cancel() {
	a.inner.Cancel()
}

// Following follows a specific broadcast across its publisher instances.
type Following struct {
	inner *ffi.MoqFollowing
}

// Available blocks until the publisher instance serving the broadcast changes and
// returns its consumer, or (nil, nil) once the path goes offline. The first call
// resolves the instance serving the path once one is announced, and is never nil.
// Each later call returns another instance taking over (a restarted publisher, or a
// replacement session), nil once the path goes offline (or, when following a pinned
// instance, once another instance takes it over), then whatever serves it next. Drop
// what was read from the previous instance then: its tracks end when its route does.
func (a *Following) Available(ctx context.Context) (*BroadcastConsumer, error) {
	inner, err := bridge.CallHandle(ctx, nil, func(ctx context.Context) (*ffi.MoqBroadcastConsumer, error) {
		res, err := a.inner.Available(ctx)
		if err != nil || res == nil {
			return nil, err
		}
		return *res, nil
	})
	if err != nil || inner == nil {
		return nil, err
	}
	return &BroadcastConsumer{inner: inner}, nil
}

// Cancel stops awaiting the broadcast.
func (a *Following) Cancel() {
	a.inner.Cancel()
}
