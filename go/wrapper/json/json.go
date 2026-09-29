// Package json publishes and consumes JSON tracks, mirroring the moq-json crate.
//
// Each type wraps a track from package moq: a producer takes over a
// [moq.TrackProducer] and advertises it in its broadcast's catalog, and a
// consumer takes over a [moq.TrackConsumer] that has not read a group yet. The
// track can come from a request as well as by name. Import it under an alias
// next to encoding/json:
//
//	import moqjson "moq.dev/moq/json"
package json

import (
	"context"
	stdjson "encoding/json"
	"iter"

	"moq.dev/moq"
	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// DefaultDeltaRatio is used when [SnapshotOptions.DeltaRatio] is nil. Go has no field
// defaults, so unlike the other bindings this restates the `#[uniffi(default = 8)]` on
// moq-ffi's MoqJsonSnapshotConfig (itself pinned to moq-json's default by a Rust test).
const DefaultDeltaRatio uint32 = 8

// SnapshotOptions configures a lossy latest-value JSON producer.
type SnapshotOptions struct {
	// DeltaRatio controls how aggressively deltas replace full snapshots. Nil uses
	// [DefaultDeltaRatio]; a pointer to zero disables deltas entirely.
	DeltaRatio *uint32
	// Compression enables group-scoped DEFLATE and must match on the consumer.
	Compression bool
}

// deltaRatio resolves the configured ratio, falling back to the shared default.
func (o SnapshotOptions) deltaRatio() uint32 {
	if o.DeltaRatio == nil {
		return DefaultDeltaRatio
	}
	return *o.DeltaRatio
}

// StreamOptions configures a lossless append-log JSON producer.
type StreamOptions struct {
	// Compression enables group-scoped DEFLATE and must match on the consumer.
	Compression bool
}

// ConsumerOptions configures reading either kind of JSON track.
//
// Delta encoding is a producer-side choice the consumer reconstructs automatically,
// so only the compression setting has to match.
type ConsumerOptions struct {
	// Compression enables group-scoped DEFLATE and must match the producer.
	Compression bool
}

// SnapshotProducer publishes a JSON value as lossy latest state.
type SnapshotProducer struct {
	inner *ffi.MoqJsonSnapshotProducer
}

// NewSnapshotProducer publishes track as a lossy latest-value JSON track, advertised in
// broadcast's catalog until it finishes. It takes over track, whose handle is closed
// afterward, and fails if the catalog already carries the track's name.
func NewSnapshotProducer(broadcast *moq.BroadcastProducer, track *moq.TrackProducer, options SnapshotOptions) (*SnapshotProducer, error) {
	inner, err := ffi.NewMoqJsonSnapshotProducer(
		bridge.BroadcastProducer(broadcast),
		bridge.TrackProducer(track),
		ffi.MoqJsonSnapshotConfig{DeltaRatio: options.deltaRatio(), Compression: options.Compression},
	)
	if err != nil {
		return nil, err
	}
	return &SnapshotProducer{inner: inner}, nil
}

// Update publishes value as a JSON snapshot or delta. It is a no-op when unchanged.
func (p *SnapshotProducer) Update(value any) error {
	encoded, err := stdjson.Marshal(value)
	if err != nil {
		return err
	}
	return p.inner.Update(string(encoded))
}

// Demand returns a watch-only handle to whether the track has subscribers.
func (p *SnapshotProducer) Demand() (*moq.TrackDemand, error) {
	inner, err := p.inner.Demand()
	if err != nil {
		return nil, err
	}
	return bridge.TrackDemand(inner).(*moq.TrackDemand), nil
}

// Finish closes the snapshot track.
func (p *SnapshotProducer) Finish() error {
	return p.inner.Finish()
}

// StreamProducer publishes an ordered lossless log of JSON records.
type StreamProducer struct {
	inner *ffi.MoqJsonStreamProducer
}

// NewStreamProducer publishes track as a lossless append-log JSON track, advertised in
// broadcast's catalog until it finishes. It takes over track, whose handle is closed
// afterward, and fails if the catalog already carries the track's name.
func NewStreamProducer(broadcast *moq.BroadcastProducer, track *moq.TrackProducer, options StreamOptions) (*StreamProducer, error) {
	inner, err := ffi.NewMoqJsonStreamProducer(
		bridge.BroadcastProducer(broadcast),
		bridge.TrackProducer(track),
		ffi.MoqJsonStreamConfig{Compression: options.Compression},
	)
	if err != nil {
		return nil, err
	}
	return &StreamProducer{inner: inner}, nil
}

// Append adds one JSON record to the log.
func (p *StreamProducer) Append(value any) error {
	encoded, err := stdjson.Marshal(value)
	if err != nil {
		return err
	}
	return p.inner.Append(string(encoded))
}

// Demand returns a watch-only handle to whether the track has subscribers.
func (p *StreamProducer) Demand() (*moq.TrackDemand, error) {
	inner, err := p.inner.Demand()
	if err != nil {
		return nil, err
	}
	return bridge.TrackDemand(inner).(*moq.TrackDemand), nil
}

// Finish closes the stream track.
func (p *StreamProducer) Finish() error {
	return p.inner.Finish()
}

// SnapshotConsumer receives the latest reconstructed JSON value.
type SnapshotConsumer struct {
	inner *ffi.MoqJsonSnapshotConsumer
}

// NewSnapshotConsumer reads track as a lossy latest-value JSON track. It takes over
// track, whose handle is closed afterward, and fails if track has already read a group.
func NewSnapshotConsumer(track *moq.TrackConsumer, options ConsumerOptions) (*SnapshotConsumer, error) {
	// DeltaRatio is producer-only; the consumer ignores it.
	inner, err := ffi.NewMoqJsonSnapshotConsumer(
		bridge.TrackConsumer(track),
		ffi.MoqJsonSnapshotConfig{Compression: options.Compression},
	)
	if err != nil {
		return nil, err
	}
	return &SnapshotConsumer{inner: inner}, nil
}

// Next returns the next JSON value, or nil when the track ends.
func (c *SnapshotConsumer) Next(ctx context.Context) (*stdjson.RawMessage, error) {
	value, err := bridge.Call(ctx, c.inner.Cancel, c.inner.Next)
	if err != nil || value == nil {
		return nil, err
	}
	decoded := stdjson.RawMessage(*value)
	return &decoded, nil
}

// Values ranges over reconstructed JSON values until the track ends.
func (c *SnapshotConsumer) Values(ctx context.Context) iter.Seq2[*stdjson.RawMessage, error] {
	return bridge.Seq(ctx, c.Next)
}

// Cancel stops current and future reads.
func (c *SnapshotConsumer) Cancel() {
	c.inner.Cancel()
}

// StreamConsumer receives every JSON record in order.
type StreamConsumer struct {
	inner *ffi.MoqJsonStreamConsumer
}

// NewStreamConsumer reads track as a lossless append-log JSON track. It takes over
// track, whose handle is closed afterward, and fails if track has already read a group.
func NewStreamConsumer(track *moq.TrackConsumer, options ConsumerOptions) (*StreamConsumer, error) {
	inner, err := ffi.NewMoqJsonStreamConsumer(
		bridge.TrackConsumer(track),
		ffi.MoqJsonStreamConfig{Compression: options.Compression},
	)
	if err != nil {
		return nil, err
	}
	return &StreamConsumer{inner: inner}, nil
}

// Next returns the next JSON record, or nil when the track ends.
func (c *StreamConsumer) Next(ctx context.Context) (*stdjson.RawMessage, error) {
	value, err := bridge.Call(ctx, c.inner.Cancel, c.inner.Next)
	if err != nil || value == nil {
		return nil, err
	}
	decoded := stdjson.RawMessage(*value)
	return &decoded, nil
}

// Values ranges over every JSON record until the track ends.
func (c *StreamConsumer) Values(ctx context.Context) iter.Seq2[*stdjson.RawMessage, error] {
	return bridge.Seq(ctx, c.Next)
}

// Cancel stops current and future reads.
func (c *StreamConsumer) Cancel() {
	c.inner.Cancel()
}
