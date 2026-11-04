package moq

import (
	"context"
	"iter"

	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// CreateDatagrams creates a live datagram track in its own namespace.
func (b *BroadcastProducer) CreateDatagrams(name string, info *DatagramInfo) (*DatagramProducer, error) {
	inner, err := b.inner.CreateDatagrams(name, info)
	if err != nil {
		return nil, err
	}
	return &DatagramProducer{inner: inner}, nil
}

// Datagrams subscribes to a live datagram track from now on, without a stream subscription.
func (b *BroadcastConsumer) Datagrams(name string) (*DatagramConsumer, error) {
	inner, err := b.inner.Datagrams(name)
	if err != nil {
		return nil, err
	}
	return &DatagramConsumer{inner: inner}, nil
}

// DatagramProducer writes datagrams with an independent sequence counter and no cache.
type DatagramProducer struct{ inner *ffi.MoqDatagramProducer }

// Name is the datagram track's name within its broadcast.
func (p *DatagramProducer) Name() (string, error) { return p.inner.Name() }

// Info returns the track's fixed publisher properties.
func (p *DatagramProducer) Info() (DatagramInfo, error) { return p.inner.Info() }

// Append sends a datagram and returns its sequence; keep payloads below the path MTU.
func (p *DatagramProducer) Append(timestamp *Timestamp, payload []byte) (uint64, error) {
	return p.inner.Append(timestamp, payload)
}

// Insert sends a datagram at its explicit sequence; appends continue past the highest one.
func (p *DatagramProducer) Insert(datagram Datagram) error { return p.inner.Insert(datagram) }

// Consume creates a local reader starting with the next datagram written.
func (p *DatagramProducer) Consume() (*DatagramConsumer, error) {
	inner, err := p.inner.Consume()
	if err != nil {
		return nil, err
	}
	return &DatagramConsumer{inner: inner}, nil
}

// Priority is the highest subscriber priority, or nil while nobody consumes this track.
func (p *DatagramProducer) Priority() (*int8, error) { return p.inner.Priority() }

// PriorityChanged waits until aggregate subscriber priority differs from last.
func (p *DatagramProducer) PriorityChanged(ctx context.Context, last *int8) (*int8, error) {
	return p.inner.PriorityChanged(ctx, last)
}

// Finish ends the track; existing consumers drain their unread datagrams.
func (p *DatagramProducer) Finish() error { return p.inner.Finish() }

// Abort ends the track with an application error and discards unread datagrams.
func (p *DatagramProducer) Abort(code uint16) error { return p.inner.Abort(code) }

// DatagramConsumer reads live datagrams in arrival order from its own cursor.
type DatagramConsumer struct{ inner *ffi.MoqDatagramConsumer }

// Name is the datagram track's name within its broadcast.
func (c *DatagramConsumer) Name() (string, error) { return c.inner.Name() }

// Info waits for the publisher to accept the track and returns its properties.
func (c *DatagramConsumer) Info(ctx context.Context) (DatagramInfo, error) {
	return bridge.Call(ctx, c.inner.Cancel, c.inner.Info)
}

// Recv returns the next datagram, or (nil, nil) after the track finishes.
func (c *DatagramConsumer) Recv(ctx context.Context) (*Datagram, error) {
	return bridge.Call(ctx, c.inner.Cancel, c.inner.Recv)
}

// All ranges over datagrams in arrival order.
func (c *DatagramConsumer) All(ctx context.Context) iter.Seq2[*Datagram, error] {
	return bridge.Seq(ctx, c.Recv)
}

// Priority is this consumer's subscriber priority.
func (c *DatagramConsumer) Priority() (int8, error) { return c.inner.Priority() }

// SetPriority changes this consumer's priority, higher first.
func (c *DatagramConsumer) SetPriority(priority int8) error { return c.inner.SetPriority(priority) }

// Cancel stops reads and releases this consumer's demand.
func (c *DatagramConsumer) Cancel() { c.inner.Cancel() }

// Datagrams creates an on-demand handler for datagram tracks.
func (r *BroadcastRequests) Datagrams() (*DatagramRequests, error) {
	inner, err := r.inner.Datagrams()
	if err != nil {
		return nil, err
	}
	return &DatagramRequests{inner: inner}, nil
}

// DatagramRequests serves on-demand datagram tracks through their own queue.
type DatagramRequests struct{ inner *ffi.MoqDatagramRequests }

// Requested waits for the next datagram track requested by a consumer.
func (r *DatagramRequests) Requested(ctx context.Context) (*DatagramRequest, error) {
	inner, err := bridge.CallHandle(ctx, r.inner.Cancel, r.inner.Requested)
	if err != nil {
		return nil, err
	}
	return &DatagramRequest{inner: inner}, nil
}

// All ranges over on-demand datagram requests.
func (r *DatagramRequests) All(ctx context.Context) iter.Seq2[*DatagramRequest, error] {
	return bridge.Seq(ctx, r.Requested)
}

// Cancel stops the handler and rejects requests it has not handed out.
func (r *DatagramRequests) Cancel() { r.inner.Cancel() }

// DatagramRequest is a requested datagram track awaiting acceptance or rejection.
type DatagramRequest struct{ inner *ffi.MoqDatagramRequest }

// Name is the requested datagram track's name.
func (r *DatagramRequest) Name() (string, error) { return r.inner.Name() }

// Priority is the highest priority among consumers waiting for this track.
func (r *DatagramRequest) Priority() (*int8, error) { return r.inner.Priority() }

// Accept accepts the request with its fixed publisher properties.
func (r *DatagramRequest) Accept(info *DatagramInfo) (*DatagramProducer, error) {
	inner, err := r.inner.Accept(info)
	if err != nil {
		return nil, err
	}
	return &DatagramProducer{inner: inner}, nil
}

// Abort rejects this request with an application error code.
func (r *DatagramRequest) Abort(code uint16) error { return r.inner.Abort(code) }
