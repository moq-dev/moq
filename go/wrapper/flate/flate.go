// Package flate publishes opaque snapshot and stream tracks.
package flate

import (
	"time"

	"moq.dev/moq"
	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// Options configures an opaque data track.
type Options struct {
	// Compression enables group-scoped DEFLATE.
	Compression bool
	// Mime describes the payload media type, or nil leaves it unstated.
	Mime *string
}

// SnapshotProducer publishes the latest opaque payload.
type SnapshotProducer struct{ inner *ffi.MoqFlateSnapshotProducer }

// NewSnapshotProducer takes over track and advertises it in broadcast's catalog.
func NewSnapshotProducer(broadcast *moq.BroadcastProducer, track *moq.TrackProducer, options Options) (*SnapshotProducer, error) {
	inner, err := ffi.NewMoqFlateSnapshotProducer(bridge.BroadcastProducer(broadcast), bridge.TrackProducer(track), ffi.MoqFlateConfig{Compression: options.Compression, Mime: options.Mime})
	if err != nil {
		return nil, err
	}
	return &SnapshotProducer{inner: inner}, nil
}

// Update publishes payload at timestamp on the broadcast clock; nil is untimed.
func (p *SnapshotProducer) Update(payload []byte, timestamp *time.Duration) error {
	at, err := bridge.OptionalDuration(timestamp, "timestamp")
	if err != nil {
		return err
	}
	return p.inner.Update(payload, at)
}

// Finish closes the track and retires its catalog entry.
func (p *SnapshotProducer) Finish() error { return p.inner.Finish() }

// StreamProducer publishes an ordered log of opaque payloads.
type StreamProducer struct{ inner *ffi.MoqFlateStreamProducer }

// NewStreamProducer takes over track and advertises it in broadcast's catalog.
func NewStreamProducer(broadcast *moq.BroadcastProducer, track *moq.TrackProducer, options Options) (*StreamProducer, error) {
	inner, err := ffi.NewMoqFlateStreamProducer(bridge.BroadcastProducer(broadcast), bridge.TrackProducer(track), ffi.MoqFlateConfig{Compression: options.Compression, Mime: options.Mime})
	if err != nil {
		return nil, err
	}
	return &StreamProducer{inner: inner}, nil
}

// Append adds payload at timestamp on the broadcast clock; nil is untimed.
func (p *StreamProducer) Append(payload []byte, timestamp *time.Duration) error {
	at, err := bridge.OptionalDuration(timestamp, "timestamp")
	if err != nil {
		return err
	}
	return p.inner.Append(payload, at)
}

// Finish closes the track and retires its catalog entry.
func (p *StreamProducer) Finish() error { return p.inner.Finish() }
