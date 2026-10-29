// Package json publishes and consumes JSON tracks, mirroring the moq-json crate.
//
// Each type wraps a track from package moq: a producer takes over a
// [moq.TrackProducer] (the snapshot and stream producers also advertise it in
// their broadcast's catalog, which has no window mode), and a consumer takes
// over a [moq.TrackConsumer] that has not read a group yet. The track can come
// from a request as well as by name. Compression is the track's: publish JSON
// with [moq.TrackInfo] Compress set, and a consumer reads it with no option of
// its own. Import it under an alias next to encoding/json:
//
//	import moqjson "moq.dev/moq/json"
package json

import (
	"context"
	stdjson "encoding/json"
	"fmt"
	"iter"
	"time"

	"moq.dev/moq"
	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// SnapshotOptions configures a lossy latest-value JSON producer.
type SnapshotOptions struct {
	// Deltas encodes updates as merge-patch deltas against the group's snapshot. Nil
	// enables them, matching the other bindings; a pointer to false publishes every
	// update as a full snapshot in its own group. With deltas on, give the track a
	// refresh ratio so a joiner past it gets the value restated in a fresh group.
	Deltas *bool
}

// deltas resolves the configured switch. Go has no field defaults, so this restates the
// `#[uniffi(default = true)]` on moq-ffi's MoqJsonSnapshotConfig (itself pinned to
// moq-json's default by a Rust test).
func (o SnapshotOptions) deltas() bool {
	return o.Deltas == nil || *o.Deltas
}

// WindowOptions configures a sliding-window JSON producer.
type WindowOptions struct {
	// CheckpointRecords bounds the records retained and restated when a group rolls,
	// for a window that is never popped: a reader that keeps up still sees every
	// record, while a later joiner gets a WindowSkip for the older ones. Nil restates
	// the whole window; zero is refused.
	CheckpointRecords *uint64
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
		ffi.MoqJsonSnapshotConfig{Deltas: options.deltas()},
	)
	if err != nil {
		return nil, err
	}
	return &SnapshotProducer{inner: inner}, nil
}

// Update publishes value at timestamp on the broadcast clock; nil is untimed.
// It is a no-op when unchanged.
func (p *SnapshotProducer) Update(value any, timestamp *time.Duration) error {
	encoded, err := stdjson.Marshal(value)
	if err != nil {
		return err
	}
	at, err := bridge.OptionalDuration(timestamp, "timestamp")
	if err != nil {
		return err
	}
	return p.inner.Update(string(encoded), at)
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
func NewStreamProducer(broadcast *moq.BroadcastProducer, track *moq.TrackProducer) (*StreamProducer, error) {
	inner, err := ffi.NewMoqJsonStreamProducer(bridge.BroadcastProducer(broadcast), bridge.TrackProducer(track))
	if err != nil {
		return nil, err
	}
	return &StreamProducer{inner: inner}, nil
}

// Append adds one JSON record at timestamp on the broadcast clock; nil is untimed.
func (p *StreamProducer) Append(value any, timestamp *time.Duration) error {
	encoded, err := stdjson.Marshal(value)
	if err != nil {
		return err
	}
	at, err := bridge.OptionalDuration(timestamp, "timestamp")
	if err != nil {
		return err
	}
	return p.inner.Append(string(encoded), at)
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

// Timed is a JSON value and the timestamp of the frame that carried it.
type Timed struct {
	// Value is the JSON document.
	Value stdjson.RawMessage
	// Timestamp is on the broadcast clock, or nil on an untimed track. It is rounded down
	// to a whole microsecond, so a tick on a track like 90 kHz may not survive a read and
	// republish exactly.
	Timestamp *time.Duration
}

func timed(value *ffi.MoqJsonValue) *Timed {
	if value == nil {
		return nil
	}
	out := &Timed{Value: stdjson.RawMessage(value.Value)}
	if value.TimestampUs != nil {
		at := time.Duration(*value.TimestampUs) * time.Microsecond
		out.Timestamp = &at
	}
	return out
}

// SnapshotConsumer receives each reconstructed JSON state.
type SnapshotConsumer struct {
	inner *ffi.MoqJsonSnapshotConsumer
}

// NewSnapshotConsumer reads track as a lossy latest-value JSON track. It takes over
// track, whose handle is closed afterward, and fails if track has already read a group.
func NewSnapshotConsumer(track *moq.TrackConsumer) (*SnapshotConsumer, error) {
	inner, err := ffi.NewMoqJsonSnapshotConsumer(bridge.TrackConsumer(track))
	if err != nil {
		return nil, err
	}
	return &SnapshotConsumer{inner: inner}, nil
}

// Next returns the next state in order, or nil when the track ends. Groups older
// than the subscription's MaxDelay are skipped, so a playhead reader sets that
// budget to its distance behind live.
func (c *SnapshotConsumer) Next(ctx context.Context) (*Timed, error) {
	value, err := bridge.Call(ctx, c.inner.Cancel, c.inner.Next)
	return timed(value), err
}

// Latest skips to the newest state, or waits for one; nil when the track ends.
// Skipped states are gone, and Next continues after this one.
func (c *SnapshotConsumer) Latest(ctx context.Context) (*Timed, error) {
	value, err := bridge.Call(ctx, c.inner.Cancel, c.inner.Latest)
	return timed(value), err
}

// All ranges over each JSON state until the track ends.
func (c *SnapshotConsumer) All(ctx context.Context) iter.Seq2[*Timed, error] {
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
func NewStreamConsumer(track *moq.TrackConsumer) (*StreamConsumer, error) {
	inner, err := ffi.NewMoqJsonStreamConsumer(bridge.TrackConsumer(track))
	if err != nil {
		return nil, err
	}
	return &StreamConsumer{inner: inner}, nil
}

// Next returns the next JSON record, or nil when the track ends.
func (c *StreamConsumer) Next(ctx context.Context) (*Timed, error) {
	value, err := bridge.Call(ctx, c.inner.Cancel, c.inner.Next)
	return timed(value), err
}

// All ranges over every JSON record until the track ends.
func (c *StreamConsumer) All(ctx context.Context) iter.Seq2[*Timed, error] {
	return bridge.Seq(ctx, c.Next)
}

// Cancel stops current and future reads.
func (c *StreamConsumer) Cancel() {
	c.inner.Cancel()
}

// WindowProducer publishes a sliding window of JSON records, pushed at the back
// and popped from the front.
type WindowProducer struct {
	inner *ffi.MoqJsonWindowProducer
}

// NewWindowProducer publishes track as a JSON window track. The window rolls to
// new groups that restate the records it retains, so a reader can join at any
// point; give the track a refresh ratio so a joiner past it gets a fresh group.
// It takes over track, whose handle is closed afterward. It is not advertised in
// the catalog, which has no window mode.
func NewWindowProducer(track *moq.TrackProducer, options WindowOptions) (*WindowProducer, error) {
	inner, err := ffi.NewMoqJsonWindowProducer(
		bridge.TrackProducer(track),
		ffi.MoqJsonWindowConfig{CheckpointRecords: options.CheckpointRecords},
	)
	if err != nil {
		return nil, err
	}
	return &WindowProducer{inner: inner}, nil
}

// Push appends one JSON record at timestamp on the broadcast clock; nil is untimed.
func (p *WindowProducer) Push(value any, timestamp *time.Duration) error {
	encoded, err := stdjson.Marshal(value)
	if err != nil {
		return err
	}
	at, err := bridge.OptionalDuration(timestamp, "timestamp")
	if err != nil {
		return err
	}
	return p.inner.Push(string(encoded), at)
}

// Pop drops count records from the front of the window, clamped to what it holds,
// at timestamp on the broadcast clock; nil is untimed.
func (p *WindowProducer) Pop(count uint64, timestamp *time.Duration) error {
	at, err := bridge.OptionalDuration(timestamp, "timestamp")
	if err != nil {
		return err
	}
	return p.inner.Pop(count, at)
}

// Cut closes the open group, so the next edit opens one restating the retained
// window. It is a no-op when no group is open. Cut at a natural boundary, such as
// the end of an utterance.
func (p *WindowProducer) Cut() error {
	return p.inner.Cut()
}

// Demand returns a watch-only handle to whether the track has subscribers.
func (p *WindowProducer) Demand() (*moq.TrackDemand, error) {
	inner, err := p.inner.Demand()
	if err != nil {
		return nil, err
	}
	return bridge.TrackDemand(inner).(*moq.TrackDemand), nil
}

// Finish closes the window track.
func (p *WindowProducer) Finish() error {
	return p.inner.Finish()
}

// WindowEvent is one change to a window: a WindowPush, WindowPop, or WindowSkip.
type WindowEvent interface {
	windowEvent()
}

// WindowPush is a record joining the window at its absolute index.
type WindowPush struct {
	// Index is the record's absolute index.
	Index uint64
	// Value is the JSON document.
	Value stdjson.RawMessage
}

// WindowPop is the records in [Start, End) leaving the window.
type WindowPop struct {
	// Start is the first index popped.
	Start uint64
	// End is one past the last index popped.
	End uint64
}

// WindowSkip is the records in [Start, End) that will never be delivered: they
// were pushed and dropped while this reader was behind, or fell outside the
// producer's checkpoint when it joined.
type WindowSkip struct {
	// Start is the first index skipped.
	Start uint64
	// End is one past the last index skipped.
	End uint64
}

func (WindowPush) windowEvent() {}
func (WindowPop) windowEvent()  {}
func (WindowSkip) windowEvent() {}

// TimedWindowEvent is a window change and the timestamp of the frame that carried it.
type TimedWindowEvent struct {
	// Event is the change to the window.
	Event WindowEvent
	// Timestamp is on the broadcast clock, or nil on an untimed track. A record
	// first learned of from a group's restatement carries that frame's time, not
	// its original push time.
	Timestamp *time.Duration
}

func timedWindowEvent(value *ffi.MoqJsonWindowTimed) (*TimedWindowEvent, error) {
	if value == nil {
		return nil, nil
	}
	out := &TimedWindowEvent{}
	switch event := value.Event.(type) {
	case ffi.MoqJsonWindowEventPush:
		out.Event = WindowPush{Index: event.Index, Value: stdjson.RawMessage(event.Value)}
	case ffi.MoqJsonWindowEventPop:
		out.Event = WindowPop{Start: event.Start, End: event.End}
	case ffi.MoqJsonWindowEventSkip:
		out.Event = WindowSkip{Start: event.Start, End: event.End}
	default:
		return nil, fmt.Errorf("unknown window event: %T", event)
	}
	if value.TimestampUs != nil {
		at := time.Duration(*value.TimestampUs) * time.Microsecond
		out.Timestamp = &at
	}
	return out, nil
}

// WindowConsumer receives each change to a JSON window in order.
type WindowConsumer struct {
	inner *ffi.MoqJsonWindowConsumer
}

// NewWindowConsumer reads track as a JSON window track. It takes over track,
// whose handle is closed afterward, and fails if track has already read a group.
func NewWindowConsumer(track *moq.TrackConsumer) (*WindowConsumer, error) {
	inner, err := ffi.NewMoqJsonWindowConsumer(bridge.TrackConsumer(track))
	if err != nil {
		return nil, err
	}
	return &WindowConsumer{inner: inner}, nil
}

// Next returns the next change to the window, or nil when the track ends. Group
// rolls never surface.
func (c *WindowConsumer) Next(ctx context.Context) (*TimedWindowEvent, error) {
	value, err := bridge.Call(ctx, c.inner.Cancel, c.inner.Next)
	if err != nil {
		return nil, err
	}
	return timedWindowEvent(value)
}

// All ranges over every change to the window until the track ends.
func (c *WindowConsumer) All(ctx context.Context) iter.Seq2[*TimedWindowEvent, error] {
	return bridge.Seq(ctx, c.Next)
}

// Cancel stops current and future reads.
func (c *WindowConsumer) Cancel() {
	c.inner.Cancel()
}
