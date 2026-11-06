// Package socket dials and accepts point-to-point sessions over hidden broadcasts.
package socket

import (
	"context"
	moq "moq.dev/moq"
	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// Endpoint announces one hidden endpoint and accepts TCP and track dials separately.
type Endpoint struct{ inner *ffi.MoqSocketEndpoint }

// NewEndpoint announces id beneath prefix, using origin to find peers.
func NewEndpoint(origin *moq.OriginProducer, prefix, id string) (*Endpoint, error) {
	inner, err := ffi.NewMoqSocketEndpoint(bridge.OriginProducer(origin), prefix, id)
	if err != nil {
		return nil, err
	}
	return &Endpoint{inner: inner}, nil
}

// ID returns this endpoint's id.
func (e *Endpoint) ID() string { return e.inner.Id() }

// Prefix returns the shared endpoint prefix.
func (e *Endpoint) Prefix() string { return e.inner.Prefix() }

// Close retracts the endpoint and resets pending dials and sessions.
func (e *Endpoint) Close() { e.inner.Close() }

// DialTCP dials a new tcp session; cancelling resets only this dial.
func (e *Endpoint) DialTCP(ctx context.Context, peer string) (*TCPStream, error) {
	inner, err := bridge.CallHandle(ctx, nil, func(ctx context.Context) (*ffi.MoqSocketTcpStream, error) { return e.inner.DialTcp(ctx, peer) })
	if err != nil {
		return nil, err
	}
	return newTCPStream(inner), nil
}

// AcceptTCP starts listening for tcp and awaits the next dial.
func (e *Endpoint) AcceptTCP(ctx context.Context) (*IncomingTCP, error) {
	inner, err := bridge.CallHandle(ctx, nil, e.inner.AcceptTcp)
	if err != nil {
		return nil, err
	}
	return &IncomingTCP{inner: inner}, nil
}

// IncomingTCP is a tcp dial, consumed by accepting or rejecting it.
type IncomingTCP struct{ inner *ffi.MoqSocketIncomingTcp }

// Peer returns the claimed endpoint; accepting verifies return routability.
func (i *IncomingTCP) Peer() string { return i.inner.Peer() }

// Session returns the private session id.
func (i *IncomingTCP) Session() string { return i.inner.Session() }

// Reject refuses the dial as not found.
func (i *IncomingTCP) Reject() error { return i.inner.Reject() }

// Accept verifies return routability and accepts this dial.
func (i *IncomingTCP) Accept(ctx context.Context) (*TCPStream, error) {
	inner, err := bridge.CallHandle(ctx, nil, func(ctx context.Context) (*ffi.MoqSocketTcpStream, error) { return i.inner.Accept(ctx) })
	if err != nil {
		return nil, err
	}
	return newTCPStream(inner), nil
}

// TCPStream exposes a tcp session's two halves and identities.
type TCPStream struct {
	// Producer is this side's write half.
	Producer *moq.GroupProducer
	// Consumer is the peer's subscribed read half.
	Consumer *moq.GroupConsumer
	// Peer is the peer endpoint's id.
	Peer string
	// Session is the private session id.
	Session string
}

func newTCPStream(inner *ffi.MoqSocketTcpStream) *TCPStream {
	defer inner.Destroy()
	return &TCPStream{
		Producer: bridge.WrapGroupProducer(inner.Producer()).(*moq.GroupProducer),
		Consumer: bridge.WrapGroupConsumer(inner.Consumer()).(*moq.GroupConsumer),
		Peer:     inner.Peer(), Session: inner.Session(),
	}
}

// DialTrack dials a new track session; cancelling resets only this dial.
func (e *Endpoint) DialTrack(ctx context.Context, peer string, info *moq.TrackInfo) (*TrackStream, error) {
	ffiInfo, err := bridge.TrackInfo(info)
	if err != nil {
		return nil, err
	}
	inner, err := bridge.CallHandle(ctx, nil, func(ctx context.Context) (*ffi.MoqSocketTrackStream, error) {
		return e.inner.DialTrack(ctx, peer, ffiInfo)
	})
	if err != nil {
		return nil, err
	}
	return newTrackStream(inner), nil
}

// AcceptTrack starts listening for track and awaits the next dial.
func (e *Endpoint) AcceptTrack(ctx context.Context) (*IncomingTrack, error) {
	inner, err := bridge.CallHandle(ctx, nil, e.inner.AcceptTrack)
	if err != nil {
		return nil, err
	}
	return &IncomingTrack{inner: inner}, nil
}

// IncomingTrack is a track dial, consumed by accepting or rejecting it.
type IncomingTrack struct{ inner *ffi.MoqSocketIncomingTrack }

// Peer returns the claimed endpoint; accepting verifies return routability.
func (i *IncomingTrack) Peer() string { return i.inner.Peer() }

// Session returns the private session id.
func (i *IncomingTrack) Session() string { return i.inner.Session() }

// Reject refuses the dial as not found.
func (i *IncomingTrack) Reject() error { return i.inner.Reject() }

// Accept verifies return routability and accepts this dial.
func (i *IncomingTrack) Accept(ctx context.Context, info *moq.TrackInfo) (*TrackStream, error) {
	ffiInfo, err := bridge.TrackInfo(info)
	if err != nil {
		return nil, err
	}
	inner, err := bridge.CallHandle(ctx, nil, func(ctx context.Context) (*ffi.MoqSocketTrackStream, error) { return i.inner.Accept(ctx, ffiInfo) })
	if err != nil {
		return nil, err
	}
	return newTrackStream(inner), nil
}

// TrackStream exposes a track session's two halves and identities.
type TrackStream struct {
	// Producer is this side's write half.
	Producer *moq.TrackProducer
	// Consumer is the peer's subscribed read half.
	Consumer *moq.TrackConsumer
	// Peer is the peer endpoint's id.
	Peer string
	// Session is the private session id.
	Session string
}

func newTrackStream(inner *ffi.MoqSocketTrackStream) *TrackStream {
	defer inner.Destroy()
	return &TrackStream{
		Producer: bridge.WrapTrackProducer(inner.Producer()).(*moq.TrackProducer),
		Consumer: bridge.WrapTrackConsumer(inner.Consumer()).(*moq.TrackConsumer),
		Peer:     inner.Peer(), Session: inner.Session(),
	}
}
