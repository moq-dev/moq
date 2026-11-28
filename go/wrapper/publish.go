package moq

import (
	"context"
	"iter"

	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

// BroadcastProducer publishes a collection of tracks. Create one at a path with
// [OriginProducer.Route] (or [Client.Route]) and take it from the [Route], then
// publish tracks onto it.
type BroadcastProducer struct {
	inner *ffi.MoqBroadcastProducer
}

// NewBroadcastProducer creates a standalone broadcast, not attached to any
// origin: pass its [BroadcastProducer.Consume] result to [BroadcastRequest.Serve]
// for a dynamic request, or use it for local pub/sub. To publish at a path, use
// [OriginProducer.Route].
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
// Tracks still open on it end with Unroutable (Internal to a moq-transport
// reader), and their later calls fail with ErrClosed; finish a track first to
// end it cleanly. Closing again is a no-op.
func (b *BroadcastProducer) Close() error {
	return b.inner.Close()
}

// BroadcastRequests is a stream of subscriber-requested tracks.
type BroadcastRequests struct {
	inner *ffi.MoqBroadcastRequests
}

// RequestedTrack waits for the next subscriber-requested track.
func (d *BroadcastRequests) RequestedTrack(ctx context.Context) (*TrackRequest, error) {
	inner, err := bridge.CallHandle(ctx, nil, d.inner.RequestedTrack)
	if err != nil {
		return nil, err
	}
	return &TrackRequest{inner: inner}, nil
}

// All ranges over subscriber-requested tracks until the dynamic source ends.
func (d *BroadcastRequests) All(ctx context.Context) iter.Seq2[*TrackRequest, error] {
	return bridge.Seq(ctx, d.inner.Cancel, d.RequestedTrack)
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
// A track asked for again, because its producer was closed while read, must be accepted with
// the info it was published with: another ends the track, and the producer's writes fail.
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

// Reject rejects the request with an application error code.
func (r *TrackRequest) Reject(errorCode uint16) error {
	return r.inner.Reject(errorCode)
}

// TrackDemand watches whether a published track has subscribers, and when they
// ask for a new group.
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

// Wanted reports whether anyone consumes the track: a subscription, fetch, or a
// group one of them is still reading.
func (d *TrackDemand) Wanted() bool {
	return d.inner.Wanted()
}

// WantedChanged blocks until Wanted changes and returns it. The first call returns at once.
// End a track nobody wants by closing its producer once this returns false: a
// reader that joined since is served by the next request for the track while a request handler is live.
func (d *TrackDemand) WantedChanged(ctx context.Context) (bool, error) {
	return d.inner.WantedChanged(ctx)
}

// Refresh blocks until a subscriber asks for a new group: the first group of a
// track with none, or a join judged by the track's TrackRefresh once its interval
// passed. It returns at once while the request is pending, and the next group
// written answers it, so loop: wait here, then write a group that starts with a
// keyframe or a snapshot.
func (d *TrackDemand) Refresh(ctx context.Context) error {
	return d.inner.Refresh(ctx)
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

// AppendGroup starts the next group with frame as its first frame, starting at the
// frame's timestamp converted to the track's timescale (on a scale-less track, Unscaled
// at the group's key); write more frames into it, then Finish. A start at or before a
// group the track holds is refused.
func (t *TrackProducer) AppendGroup(frame Frame) (*GroupProducer, error) {
	inner, err := t.inner.AppendGroup(frame)
	if err != nil {
		return nil, err
	}
	return &GroupProducer{inner: inner}, nil
}

// CreateGroup creates a group at an explicit start, its first frame's timestamp, for
// sparse or replayed groups.
func (t *TrackProducer) CreateGroup(frame Frame) (*GroupProducer, error) {
	inner, err := t.inner.CreateGroup(frame)
	if err != nil {
		return nil, err
	}
	return &GroupProducer{inner: inner}, nil
}

// SkipGroups declares that groups start up to the exclusive end will never be
// produced, so readers move past them. Creating a group there then fails.
func (t *TrackProducer) SkipGroups(start, end uint64) error {
	return t.inner.SkipGroups(start, end)
}

// WriteFrame writes frame as a single-frame group, which starts at its timestamp: on a
// scale-less track, moq.Unscaled at the new group's key.
func (t *TrackProducer) WriteFrame(frame Frame) error {
	return t.inner.WriteFrame(frame)
}

// Largest reports the newest group written, at its newest frame, or nil
// before any.
func (t *TrackProducer) Largest() (*Position, error) {
	return t.inner.Largest()
}

// Abort closes the track with an application error code.
func (t *TrackProducer) Abort(errorCode uint16) error {
	return t.inner.Abort(errorCode)
}

// Close releases the producer on the calling goroutine without finishing its track: a
// track nobody wants ends cleanly, and a reader that joined since is served by the next
// request for it while a request handler is live. The producer is unusable afterwards.
func (t *TrackProducer) Close() { t.inner.Destroy() }

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

// FinishAt declares the exclusive final group start ahead of the live edge.
// The producer remains open for groups below the boundary.
func (t *TrackProducer) FinishAt(finalStart uint64) error {
	return t.inner.FinishAt(finalStart)
}

// GroupProducer writes frames into a single group on a track.
type GroupProducer struct {
	inner *ffi.MoqGroupProducer
}

// Start is this group's start within the track.
func (g *GroupProducer) Start() uint64 {
	return g.inner.Start()
}

// Consume reads frames from this group.
func (g *GroupProducer) Consume() (*GroupConsumer, error) {
	inner, err := g.inner.Consume()
	if err != nil {
		return nil, err
	}
	return &GroupConsumer{inner: inner}, nil
}

// Ready blocks until a frame of size bytes fits a window ahead of the fastest
// reader. Call it before each WriteFrame so that reader paces the writer, as TCP
// backpressure does, instead of the group shedding frames it has not taken, which
// fails it with Evicted; slower readers are not waited on. It also waits until a reader holds
// the group, and returns once the group is closed so the next write reports why.
// It fails with Closed after Abort.
func (g *GroupProducer) Ready(ctx context.Context, size uint64) error {
	return g.inner.Ready(ctx, size)
}

// WriteFrame appends frame to the group. It never blocks: without Ready, a writer
// that outpaces its readers sheds the front of the group instead. On a scale-less track
// the frame's timestamp is Unscaled at this group's Start.
func (g *GroupProducer) WriteFrame(frame Frame) error {
	return g.inner.WriteFrame(frame)
}

// Finish marks the group complete. The handle remains so Abort can still run.
func (g *GroupProducer) Finish() error {
	return g.inner.Finish()
}

// FinishAt marks the group complete with its exclusive end in the track's timescale.
// No other group may start before end. The handle remains so Abort can still run.
func (g *GroupProducer) FinishAt(end uint64) error {
	return g.inner.FinishAt(end)
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
	inner, err := bridge.CallHandle(ctx, nil, d.inner.RequestedFill)
	if err != nil {
		return nil, err
	}
	return &Fill{inner: inner}, nil
}

// All ranges over fills until the dynamic source ends.
func (d *TrackRequests) All(ctx context.Context) iter.Seq2[*Fill, error] {
	return bridge.Seq(ctx, d.inner.Cancel, d.RequestedFill)
}

// Cancel stops current and future requested-fill waits.
func (d *TrackRequests) Cancel() {
	d.inner.Cancel()
}

// Fill is a run of groups readers miss, for a TrackRequests to serve.
//
// Accept each group you have and DropGroups the rest, one group at a time as
// WantedChanged returns them; Wanted names what is still owed, in the fill's
// order. Releasing the fill names whatever is left as dropped
// while a reader still wants it; once nobody does, it is withdrawn, so stop
// serving it once Wanted is nil. Stop serving a single group when Unwanted
// returns for it, and move on to Wanted. Serve the fills you take
// concurrently: each waits on its own readers.
type Fill struct {
	inner *ffi.MoqFill
}

// Start is the first group to fill, and the frame within it to start from.
func (f *Fill) Start() Position {
	return f.inner.Start()
}

// End is the exclusive end group.
func (f *Fill) End() uint64 {
	return f.inner.End()
}

// Priority is the most urgent delivery priority among the readers waiting on it.
func (f *Fill) Priority() int8 {
	return f.inner.Priority()
}

// Order is which end to fill first.
func (f *Fill) Order() Order {
	return f.inner.Order()
}

// Wanted is the next group still owed in the fill's order, or nil once every
// group is answered or no reader waits on the fill. It does not wait for the
// readers; WantedChanged does.
func (f *Fill) Wanted() *uint64 {
	return f.inner.Wanted()
}

// WantedChanged blocks until Wanted changed and every group this fill accepted
// reached a reader, which picks it up when it next reads, and returns it. Nil,
// once the fill is done, never waits; the first call returns at once unless an
// accepted group has yet to reach a reader.
func (f *Fill) WantedChanged(ctx context.Context) (*uint64, error) {
	return f.inner.WantedChanged(ctx)
}

// Accept inserts group start and returns a producer for it. The first group
// starts at Start's frame, so its frames keep their indices.
func (f *Fill) Accept(start uint64) (*GroupProducer, error) {
	inner, err := f.inner.Accept(start)
	if err != nil {
		return nil, err
	}
	return &GroupProducer{inner: inner}, nil
}

// DropGroups names groups start up to the exclusive end as never to be delivered.
func (f *Fill) DropGroups(start, end uint64) {
	f.inner.DropGroups(start, end)
}

// IsWanted reports whether a reader still waits for group start from this fill:
// false once every reader took it or narrowed past it, or nobody waits on the fill.
func (f *Fill) IsWanted(start uint64) bool {
	return f.inner.IsWanted(start)
}

// Unwanted blocks until no reader waits for group start from this fill, so stop
// serving it.
func (f *Fill) Unwanted(ctx context.Context, start uint64) error {
	return f.inner.Unwanted(ctx, start)
}
