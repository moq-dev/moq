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
// (each producer, published broadcast, and [OriginRequests]), and its consumers
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

// Requests advertises prefix and serves the requests beneath it.
//
// A route claims prefix and every path beneath it ("" claims every path). A
// service that only serves some of them rejects the rest as they are requested.
// Create, Requests if tracks are served on demand, populate, then
// [Publication.Announce].
func (o *OriginProducer) Requests(prefix string) (*OriginRequests, error) {
	inner, err := o.inner.Requests(prefix)
	if err != nil {
		return nil, err
	}
	return &OriginRequests{inner: inner}, nil
}

// CreateBroadcast creates a broadcast at the given path, returning its publication.
//
// The broadcast is invisible and unroutable, for this origin's consumers and
// peers alike, until [Publication.Announce]. Announce it after populating
// tracks. [BroadcastProducer.Close] ends it for good; dropping the last handle,
// the publication included, does the same.
func (o *OriginProducer) CreateBroadcast(path string) (*Publication, error) {
	inner, err := o.inner.CreateBroadcast(path)
	if err != nil {
		return nil, err
	}
	return newPublication(inner), nil
}

// Publication is a broadcast published on an origin: the broadcast to fill and
// its announcement.
//
// The collector reclaiming it does not retract the route; [BroadcastProducer.Close] does.
type Publication struct {
	inner     *ffi.MoqPublication
	broadcast *BroadcastProducer
}

func newPublication(inner *ffi.MoqPublication) *Publication {
	return &Publication{inner: inner, broadcast: &BroadcastProducer{inner: inner.Broadcast()}}
}

// Broadcast returns the broadcast to fill, the same one on every call.
func (p *Publication) Broadcast() *BroadcastProducer {
	return p.broadcast
}

// Announce advertises the broadcast's exact path as a route.
//
// Announcing again re-prices the route in place. Until announced, the
// broadcast is invisible and unroutable for local consumers and peers alike.
// Errors with ErrClosed once the broadcast is closed.
func (p *Publication) Announce(route AnnounceRoute) error {
	return p.inner.Announce(route)
}

// Unannounce retracts the broadcast's exact-path advertisement, if any, from
// local consumers and peers alike. Tracks already in flight carry on, and
// Announce brings it back. Errors with ErrClosed once the broadcast is closed.
func (p *Publication) Unannounce() error {
	return p.inner.Unannounce()
}

// OriginRequests streams requests for paths with no existing exact-path broadcast.
type OriginRequests struct {
	inner *ffi.MoqOriginRequests
}

// RequestedBroadcast blocks until a consumer requests a path with no existing
// exact-path broadcast.
func (d *OriginRequests) RequestedBroadcast(ctx context.Context) (*BroadcastRequest, error) {
	inner, err := bridge.CallHandle(ctx, d.inner.Cancel, d.inner.RequestedBroadcast)
	if err != nil {
		return nil, err
	}
	return &BroadcastRequest{inner: inner}, nil
}

// All ranges over requested broadcasts until the stream errors or the loop
// breaks.
func (d *OriginRequests) All(ctx context.Context) iter.Seq2[*BroadcastRequest, error] {
	return bridge.Seq(ctx, d.RequestedBroadcast)
}

// Cancel stops serving requested broadcasts and retracts the route.
func (d *OriginRequests) Cancel() {
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

// Prepare starts an answer under a new epoch, hidden until published.
func (r *BroadcastRequest) Prepare(epoch string) (*BroadcastPrepared, error) {
	inner, err := r.inner.Prepare(epoch)
	if err != nil {
		return nil, err
	}
	return &BroadcastPrepared{inner: inner}, nil
}

// Reject fails the request with an application error code.
func (r *BroadcastRequest) Reject(errorCode uint16) error {
	return r.inner.Reject(errorCode)
}

// BroadcastPrepared is a claim answer being filled before announcement.
type BroadcastPrepared struct {
	inner *ffi.MoqBroadcastPrepared
}

// Broadcast returns the unannounced broadcast to fill before publishing; only Publish announces it.
func (p *BroadcastPrepared) Broadcast() *BroadcastProducer {
	return &BroadcastProducer{inner: p.inner.Broadcast()}
}

// Publish announces the answer and resolves every waiting requester, returning
// its publication.
func (p *BroadcastPrepared) Publish() (*Publication, error) {
	inner, err := p.inner.Publish()
	if err != nil {
		return nil, err
	}
	return newPublication(inner), nil
}

// Close abandons the answer if it was not published, refusing every waiting requester.
func (p *BroadcastPrepared) Close() {
	p.inner.Destroy()
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

// Follow follows an exact path, resolving each publisher instance that serves
// it. A non-nil epoch (UUIDv7 text) follows only that instance.
func (o *OriginConsumer) Follow(path string, epoch *string) (*Following, error) {
	inner, err := o.inner.Follow(path, epoch)
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
// does not wait for a future announcement. A non-nil epoch (UUIDv7 text)
// refuses any other instance. Blocks until resolved.
func (o *OriginConsumer) Request(ctx context.Context, path string, epoch *string) (*BroadcastConsumer, error) {
	inner, err := o.inner.Request(ctx, path, epoch)
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
