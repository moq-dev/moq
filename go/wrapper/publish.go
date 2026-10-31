package moq

import (
	"context"
	"iter"
	"time"

	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// BroadcastProducer publishes a collection of tracks. Create one at a path with
// [OriginProducer.CreateBroadcast] (or [Client.CreateBroadcast]) and take it
// from the [Publication], then publish tracks onto it.
type BroadcastProducer struct {
	inner *ffi.MoqBroadcastProducer
}

// NewBroadcastProducer creates a standalone broadcast, not attached to any
// origin: use it to serve a dynamic request ([BroadcastRequest.Accept]) or for
// local pub/sub. To publish at a path, use [OriginProducer.CreateBroadcast].
func NewBroadcastProducer() (*BroadcastProducer, error) {
	inner, err := ffi.NewMoqBroadcastProducer()
	if err != nil {
		return nil, err
	}
	return &BroadcastProducer{inner: inner}, nil
}

// Requests accepts requests for tracks that are not published yet.
func (b *BroadcastProducer) Requests() (*BroadcastRequests, error) {
	inner, err := b.inner.Requests()
	if err != nil {
		return nil, err
	}
	return &BroadcastRequests{inner: inner}, nil
}

// PublishTrack creates a track that carries arbitrary byte payloads with no
// codec validation. info sets track properties (priority, cache, timescale);
// pass nil for defaults.
func (b *BroadcastProducer) PublishTrack(name string, info *TrackInfo) (*TrackProducer, error) {
	ffiInfo, err := trackInfoFFI(info)
	if err != nil {
		return nil, err
	}
	inner, err := b.inner.PublishTrack(name, ffiInfo)
	if err != nil {
		return nil, err
	}
	return &TrackProducer{inner: inner}, nil
}

// Consume returns a consumer that reads from this broadcast's tracks.
func (b *BroadcastProducer) Consume() (*BroadcastConsumer, error) {
	inner, err := b.inner.Consume()
	if err != nil {
		return nil, err
	}
	return &BroadcastConsumer{inner: inner}, nil
}

// Close ends the broadcast for good: it retracts and serves no new tracks.
// Tracks still open on it end with Unroutable, which readers take as the
// publisher leaving, and their later calls fail with ErrClosed; finish a track
// first to end it cleanly. Closing again is a no-op.
func (b *BroadcastProducer) Close() error {
	return b.inner.Close()
}

// BroadcastRequests is a stream of subscriber-requested tracks.
type BroadcastRequests struct {
	inner *ffi.MoqBroadcastRequests
}

// RequestedTrack waits for the next subscriber-requested track.
func (d *BroadcastRequests) RequestedTrack(ctx context.Context) (*TrackRequest, error) {
	inner, err := bridge.CallHandle(ctx, d.inner.Cancel, d.inner.RequestedTrack)
	if err != nil {
		return nil, err
	}
	return &TrackRequest{inner: inner}, nil
}

// All ranges over subscriber-requested tracks until the dynamic source ends.
func (d *BroadcastRequests) All(ctx context.Context) iter.Seq2[*TrackRequest, error] {
	return bridge.Seq(ctx, d.RequestedTrack)
}

// Cancel stops the dynamic request stream.
func (d *BroadcastRequests) Cancel() {
	d.inner.Cancel()
}

// TrackRequest is a subscriber-requested track that has not been accepted yet.
type TrackRequest struct {
	inner *ffi.MoqTrackRequest
}

// Name is the requested track name.
func (r *TrackRequest) Name() (string, error) {
	return r.inner.Name()
}

// Demand watches this track's subscribers, including after accepting it.
func (r *TrackRequest) Demand() (*TrackDemand, error) {
	inner, err := r.inner.Demand()
	if err != nil {
		return nil, err
	}
	return &TrackDemand{inner: inner}, nil
}

// Requests creates a fetch handler before accepting this requested track.
func (r *TrackRequest) Requests() (*TrackRequests, error) {
	inner, err := r.inner.Requests()
	if err != nil {
		return nil, err
	}
	return &TrackRequests{inner: inner}, nil
}

// Accept accepts the request as a raw track. For media, pass the request as media.Requested to an importer constructor.
func (r *TrackRequest) Accept(info *TrackInfo) (*TrackProducer, error) {
	ffiInfo, err := trackInfoFFI(info)
	if err != nil {
		return nil, err
	}
	inner, err := r.inner.Accept(ffiInfo)
	if err != nil {
		return nil, err
	}
	return &TrackProducer{inner: inner}, nil
}

// Abort rejects the request with an application error code.
func (r *TrackRequest) Abort(errorCode uint16) error {
	return r.inner.Abort(errorCode)
}

// TrackDemand watches the group a published track's subscribers want next.
//
// It is weak: holding it neither keeps the track open nor locks the producer, so
// a wait can park here while the producer keeps publishing. Waits return
// ErrClosed once the track is released.
type TrackDemand struct {
	inner *ffi.MoqTrackDemand
}

// Name is the name of the track this watches.
func (d *TrackDemand) Name() string {
	return d.inner.Name()
}

// Wanted is the group subscribers want next, or nil while nobody subscribes. A
// value past the newest group asks for a new group now, typically a keyframe.
func (d *TrackDemand) Wanted() *uint64 {
	return d.inner.Wanted()
}

// WantedChanged blocks until Wanted changes and returns it. The first call returns at once.
func (d *TrackDemand) WantedChanged(ctx context.Context) (*uint64, error) {
	return d.inner.WantedChanged(ctx)
}

// TrackProducer writes arbitrary byte payloads with no codec required.
type TrackProducer struct {
	inner *ffi.MoqTrackProducer
}

// Demand returns a watch-only handle to the track's name and the group its subscribers want.
func (t *TrackProducer) Demand() (*TrackDemand, error) {
	inner, err := t.inner.Demand()
	if err != nil {
		return nil, err
	}
	return &TrackDemand{inner: inner}, nil
}

// Requests serves fetches for groups that are not currently cached.
func (t *TrackProducer) Requests() (*TrackRequests, error) {
	inner, err := t.inner.Requests()
	if err != nil {
		return nil, err
	}
	return &TrackRequests{inner: inner}, nil
}

// AppendGroup starts a new group; write frames into it, then Finish.
func (t *TrackProducer) AppendGroup() (*GroupProducer, error) {
	inner, err := t.inner.AppendGroup()
	if err != nil {
		return nil, err
	}
	return &GroupProducer{inner: inner}, nil
}

// CreateGroup creates a group with an explicit sequence number.
func (t *TrackProducer) CreateGroup(sequence uint64) (*GroupProducer, error) {
	inner, err := t.inner.CreateGroup(sequence)
	if err != nil {
		return nil, err
	}
	return &GroupProducer{inner: inner}, nil
}

// WriteFrame writes frame as a single-frame group.
func (t *TrackProducer) WriteFrame(frame Frame) error {
	f, err := frame.ffi()
	if err != nil {
		return err
	}
	return t.inner.WriteFrame(f)
}

// AppendDatagram sends frame as a best-effort datagram and returns the sequence number
// assigned to it. Payloads are capped at 1200 bytes. There is no stream fallback.
func (t *TrackProducer) AppendDatagram(frame Frame) (uint64, error) {
	f, err := frame.ffi()
	if err != nil {
		return 0, err
	}
	return t.inner.AppendDatagram(f)
}

// InsertDatagram sends frame as a best-effort datagram at an explicit sequence, for a
// caller that allocates its own. AppendDatagram continues after the largest one.
func (t *TrackProducer) InsertDatagram(sequence uint64, frame Frame) error {
	f, err := frame.ffi()
	if err != nil {
		return err
	}
	return t.inner.InsertDatagram(sequence, f)
}

// Largest reports the newest group or datagram written, at its newest frame, or nil
// before any.
func (t *TrackProducer) Largest() (*Position, error) {
	return t.inner.Largest()
}

// Abort closes the track with an application error code.
func (t *TrackProducer) Abort(errorCode uint16) error {
	return t.inner.Abort(errorCode)
}

// Consume reads directly from this producer's track. subscription tunes delivery
// (delivery priority, group range); pass nil for defaults.
func (t *TrackProducer) Consume(subscription *Subscription) (*TrackConsumer, error) {
	sub, err := subscriptionFFI(subscription)
	if err != nil {
		return nil, err
	}
	inner, err := t.inner.Consume(sub)
	if err != nil {
		return nil, err
	}
	return &TrackConsumer{inner: inner}, nil
}

// Finish ends the track at the live edge. The handle remains so Abort can still run.
func (t *TrackProducer) Finish() error {
	return t.inner.Finish()
}

// FinishAt declares the exclusive final group sequence ahead of the live edge.
// The producer remains open for groups below the boundary.
func (t *TrackProducer) FinishAt(finalSequence uint64) error {
	return t.inner.FinishAt(finalSequence)
}

// GroupProducer writes frames into a single group on a track.
type GroupProducer struct {
	inner *ffi.MoqGroupProducer
}

// Sequence is this group's sequence number within the track.
func (g *GroupProducer) Sequence() uint64 {
	return g.inner.Sequence()
}

// Consume reads frames from this group.
func (g *GroupProducer) Consume() (*GroupConsumer, error) {
	inner, err := g.inner.Consume()
	if err != nil {
		return nil, err
	}
	return &GroupConsumer{inner: inner}, nil
}

// WriteFrame appends frame to the group.
func (g *GroupProducer) WriteFrame(frame Frame) error {
	f, err := frame.ffi()
	if err != nil {
		return err
	}
	return g.inner.WriteFrame(f)
}

// Finish marks the group complete. The handle remains so Abort can still run.
func (g *GroupProducer) Finish() error {
	return g.inner.Finish()
}

// Abort terminates the group with an application error code.
func (g *GroupProducer) Abort(errorCode uint16) error {
	return g.inner.Abort(errorCode)
}

// TrackRequests yields the fills readers of one track need: runs of groups the cache lacks.
type TrackRequests struct {
	inner *ffi.MoqTrackRequests
}

// RequestedFill waits for the next fill.
func (d *TrackRequests) RequestedFill(ctx context.Context) (*Fill, error) {
	inner, err := bridge.CallHandle(ctx, d.inner.Cancel, d.inner.RequestedFill)
	if err != nil {
		return nil, err
	}
	return &Fill{inner: inner}, nil
}

// All ranges over fills until the dynamic source ends.
func (d *TrackRequests) All(ctx context.Context) iter.Seq2[*Fill, error] {
	return bridge.Seq(ctx, d.RequestedFill)
}

// Cancel stops current and future requested-fill waits.
func (d *TrackRequests) Cancel() {
	d.inner.Cancel()
}

// Fill is a run of groups readers miss, for a TrackRequests to serve.
//
// Accept each group you have and DropGroups the rest; Next names what is still
// owed, in the fill's order. Releasing the fill names whatever is left as dropped
// while a reader still wants it; once nobody does, it is withdrawn, so stop
// serving it when Unused returns. Stop serving a single group when Unwanted
// returns for it, and move on to Next.
type Fill struct {
	inner *ffi.MoqFill
}

// StartGroup is the first group to fill.
func (f *Fill) StartGroup() uint64 {
	return f.inner.StartGroup()
}

// StartFrame is the frame within the first group to start from.
func (f *Fill) StartFrame() uint64 {
	return f.inner.StartFrame()
}

// EndGroup is the exclusive end group.
func (f *Fill) EndGroup() uint64 {
	return f.inner.EndGroup()
}

// Priority is the most urgent delivery priority among the readers waiting on it.
func (f *Fill) Priority() int8 {
	return f.inner.Priority()
}

// MaxDelay is the most tolerant age budget among its readers, or nil for the
// publisher's max age alone.
func (f *Fill) MaxDelay() *time.Duration {
	return f.inner.MaxDelay()
}

// Order is which end to fill first.
func (f *Fill) Order() Order {
	return f.inner.Order()
}

// Next is the next group still owed in the fill's order, or nil once every group is answered.
func (f *Fill) Next() *uint64 {
	return f.inner.Next()
}

// Accept inserts group sequence and returns a producer for it. The first group
// starts at StartFrame, so its frames keep their indices.
func (f *Fill) Accept(sequence uint64) (*GroupProducer, error) {
	inner, err := f.inner.Accept(sequence)
	if err != nil {
		return nil, err
	}
	return &GroupProducer{inner: inner}, nil
}

// DropGroups names groups start up to the exclusive end as never to be delivered.
func (f *Fill) DropGroups(start, end uint64) {
	f.inner.DropGroups(start, end)
}

// IsUsed reports whether a reader still waits on this fill.
func (f *Fill) IsUsed() bool {
	return f.inner.IsUsed()
}

// Unused blocks until no reader waits on this fill.
func (f *Fill) Unused(ctx context.Context) error {
	return f.inner.Unused(ctx)
}

// IsWanted reports whether a reader still waits for group sequence from this fill:
// false once every reader took it or narrowed past it, or nobody waits on the fill.
func (f *Fill) IsWanted(sequence uint64) bool {
	return f.inner.IsWanted(sequence)
}

// Unwanted blocks until no reader waits for group sequence from this fill, so stop
// serving it.
func (f *Fill) Unwanted(ctx context.Context, sequence uint64) error {
	return f.inner.Unwanted(ctx, sequence)
}
