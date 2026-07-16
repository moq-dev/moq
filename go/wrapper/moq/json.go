package moq

import (
	"context"
	"encoding/json"
	"iter"

	ffi "github.com/moq-dev/moq-go-ffi/moq"
)

// JSONSnapshotOptions configures a lossy latest-value JSON track.
type JSONSnapshotOptions struct {
	// DeltaRatio controls how aggressively deltas replace full snapshots. Zero disables deltas.
	DeltaRatio uint32
	// Compression enables group-scoped DEFLATE and must match on the consumer.
	Compression bool
}

// JSONStreamOptions configures a lossless append-log JSON track.
type JSONStreamOptions struct {
	// Compression enables group-scoped DEFLATE and must match on the consumer.
	Compression bool
}

// JSONSnapshotProducer publishes a JSON value as lossy latest state.
type JSONSnapshotProducer struct {
	inner *ffi.MoqJsonSnapshotProducer
}

// Update publishes value as a JSON snapshot or delta. It is a no-op when unchanged.
func (p *JSONSnapshotProducer) Update(value any) error {
	encoded, err := json.Marshal(value)
	if err != nil {
		return err
	}
	return p.inner.Update(string(encoded))
}

// Finish closes the snapshot track.
func (p *JSONSnapshotProducer) Finish() error {
	return p.inner.Finish()
}

// JSONStreamProducer publishes an ordered lossless log of JSON records.
type JSONStreamProducer struct {
	inner *ffi.MoqJsonStreamProducer
}

// Append adds one JSON record to the log.
func (p *JSONStreamProducer) Append(value any) error {
	encoded, err := json.Marshal(value)
	if err != nil {
		return err
	}
	return p.inner.Append(string(encoded))
}

// Finish closes the stream track.
func (p *JSONStreamProducer) Finish() error {
	return p.inner.Finish()
}

// JSONSnapshotConsumer receives the latest reconstructed JSON value.
type JSONSnapshotConsumer struct {
	inner *ffi.MoqJsonSnapshotConsumer
}

// Next returns the next JSON value, or nil when the track ends.
func (c *JSONSnapshotConsumer) Next(ctx context.Context) (*json.RawMessage, error) {
	value, err := runCancellable(ctx, c.inner.Cancel, c.inner.Next)
	if err != nil || value == nil {
		return nil, err
	}
	decoded := json.RawMessage(*value)
	return &decoded, nil
}

// Values ranges over reconstructed JSON values until the track ends.
func (c *JSONSnapshotConsumer) Values(ctx context.Context) iter.Seq2[*json.RawMessage, error] {
	return streamSeq(ctx, c.Next)
}

// Cancel stops current and future reads.
func (c *JSONSnapshotConsumer) Cancel() {
	c.inner.Cancel()
}

// JSONStreamConsumer receives every JSON record in order.
type JSONStreamConsumer struct {
	inner *ffi.MoqJsonStreamConsumer
}

// Next returns the next JSON record, or nil when the track ends.
func (c *JSONStreamConsumer) Next(ctx context.Context) (*json.RawMessage, error) {
	value, err := runCancellable(ctx, c.inner.Cancel, c.inner.Next)
	if err != nil || value == nil {
		return nil, err
	}
	decoded := json.RawMessage(*value)
	return &decoded, nil
}

// Values ranges over every JSON record until the track ends.
func (c *JSONStreamConsumer) Values(ctx context.Context) iter.Seq2[*json.RawMessage, error] {
	return streamSeq(ctx, c.Next)
}

// Cancel stops current and future reads.
func (c *JSONStreamConsumer) Cancel() {
	c.inner.Cancel()
}

// PublishJSONSnapshot opens a lossy latest-value JSON track.
func (b *BroadcastProducer) PublishJSONSnapshot(
	name string,
	options JSONSnapshotOptions,
) (*JSONSnapshotProducer, error) {
	inner, err := b.inner.PublishJsonSnapshot(name, ffi.MoqJsonSnapshotConfig{
		DeltaRatio:  options.DeltaRatio,
		Compression: options.Compression,
	})
	if err != nil {
		return nil, err
	}
	return &JSONSnapshotProducer{inner: inner}, nil
}

// PublishJSONStream opens a lossless append-log JSON track.
func (b *BroadcastProducer) PublishJSONStream(name string, options JSONStreamOptions) (*JSONStreamProducer, error) {
	inner, err := b.inner.PublishJsonStream(name, ffi.MoqJsonStreamConfig{Compression: options.Compression})
	if err != nil {
		return nil, err
	}
	return &JSONStreamProducer{inner: inner}, nil
}

// SubscribeJSONSnapshot subscribes to a lossy latest-value JSON track.
func (b *BroadcastConsumer) SubscribeJSONSnapshot(
	name string,
	options JSONSnapshotOptions,
) (*JSONSnapshotConsumer, error) {
	inner, err := b.inner.SubscribeJsonSnapshot(name, ffi.MoqJsonSnapshotConfig{
		DeltaRatio:  0,
		Compression: options.Compression,
	})
	if err != nil {
		return nil, err
	}
	return &JSONSnapshotConsumer{inner: inner}, nil
}

// SubscribeJSONStream subscribes to a lossless append-log JSON track.
func (b *BroadcastConsumer) SubscribeJSONStream(
	name string,
	options JSONStreamOptions,
) (*JSONStreamConsumer, error) {
	inner, err := b.inner.SubscribeJsonStream(name, ffi.MoqJsonStreamConfig{Compression: options.Compression})
	if err != nil {
		return nil, err
	}
	return &JSONStreamConsumer{inner: inner}, nil
}
