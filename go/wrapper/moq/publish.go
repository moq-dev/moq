package moq

import (
	"context"
	"errors"
	"iter"

	ffi "github.com/moq-dev/moq-go-ffi/moq"
)

// MediaOption configures media tracks published by a BroadcastProducer.
type MediaOption func(*mediaConfig)

type mediaConfig struct {
	video *VideoHint
}

var errNilTrackRequest = errors.New("moq: nil track request")

// WithVideoHint seeds catalog fields that a video stream cannot reveal itself.
func WithVideoHint(hint VideoHint) MediaOption {
	return func(c *mediaConfig) {
		h := hint
		c.video = &h
	}
}

func mediaInit(format string, init []byte, opts []MediaOption) ffi.MoqInit {
	var cfg mediaConfig
	for _, opt := range opts {
		opt(&cfg)
	}
	return ffi.MoqInit{Format: format, Data: init, Video: cfg.video}
}

// BroadcastProducer publishes a collection of tracks. Build one, publish tracks
// onto it, then publish the broadcast itself to an origin/client/server.
type BroadcastProducer struct {
	inner *ffi.MoqBroadcastProducer
}

// NewBroadcastProducer creates an empty broadcast.
func NewBroadcastProducer() (*BroadcastProducer, error) {
	inner, err := ffi.NewMoqBroadcastProducer()
	if err != nil {
		return nil, err
	}
	return &BroadcastProducer{inner: inner}, nil
}

// Dynamic accepts requests for tracks that are not published yet.
func (b *BroadcastProducer) Dynamic() (*BroadcastDynamic, error) {
	inner, err := b.inner.Dynamic()
	if err != nil {
		return nil, err
	}
	return &BroadcastDynamic{inner: inner}, nil
}

// PublishMedia publishes a media track from an init segment, fed frame by
// frame with explicit timestamps.
func (b *BroadcastProducer) PublishMedia(format string, init []byte, opts ...MediaOption) (*MediaProducer, error) {
	inner, err := b.inner.PublishMedia(mediaInit(format, init, opts))
	if err != nil {
		return nil, err
	}
	return &MediaProducer{inner: inner}, nil
}

// PublishMediaOnTrack publishes media onto a subscriber-requested track.
func (b *BroadcastProducer) PublishMediaOnTrack(request *TrackRequest, format string, init []byte, opts ...MediaOption) (*MediaProducer, error) {
	if request == nil {
		return nil, errNilTrackRequest
	}
	inner, err := b.inner.PublishMediaOnTrack(request.inner, mediaInit(format, init, opts))
	if err != nil {
		return nil, err
	}
	return &MediaProducer{inner: inner}, nil
}

// PublishMediaStream publishes a media track fed by a raw byte stream with
// unknown frame boundaries (e.g. Annex-B H.264). format is a stream format:
// avc3, hev1, av01, fmp4, or mkv.
func (b *BroadcastProducer) PublishMediaStream(format string, opts ...MediaOption) (*MediaStreamProducer, error) {
	inner, err := b.inner.PublishMediaStream(mediaInit(format, nil, opts))
	if err != nil {
		return nil, err
	}
	return &MediaStreamProducer{inner: inner}, nil
}

// PublishAudio publishes a raw-audio track with an in-process Opus encoder.
func (b *BroadcastProducer) PublishAudio(name string, input AudioEncoderInput, output AudioEncoderOutput) (*AudioProducer, error) {
	inner, err := b.inner.PublishAudio(name, input, output)
	if err != nil {
		return nil, err
	}
	return &AudioProducer{inner: inner}, nil
}

// PublishTrack creates a track that carries arbitrary byte payloads with no
// codec validation. info sets track properties (priority, cache, timescale);
// pass nil for defaults.
func (b *BroadcastProducer) PublishTrack(name string, info *TrackInfo) (*TrackProducer, error) {
	inner, err := b.inner.PublishTrack(name, info)
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

// SetCatalogSection sets (or replaces) an untyped application catalog section by
// name. json is any JSON document as a string; it rides alongside video/audio and
// reaches subscribers via Catalog.Sections. name must not be a reserved media
// section ("video"/"audio"). The catalog is republished automatically.
func (b *BroadcastProducer) SetCatalogSection(name string, json string) error {
	return b.inner.SetCatalogSection(name, json)
}

// RemoveCatalogSection removes an untyped application catalog section by name. It
// is a no-op if the section was absent.
func (b *BroadcastProducer) RemoveCatalogSection(name string) error {
	return b.inner.RemoveCatalogSection(name)
}

// Finish closes the broadcast.
func (b *BroadcastProducer) Finish() error {
	return b.inner.Finish()
}

// BroadcastDynamic is a stream of subscriber-requested tracks.
type BroadcastDynamic struct {
	inner *ffi.MoqBroadcastDynamic
}

// RequestedTrack waits for the next subscriber-requested track.
func (d *BroadcastDynamic) RequestedTrack(ctx context.Context) (*TrackRequest, error) {
	inner, err := runCancellable(ctx, d.inner.Cancel, d.inner.RequestedTrack)
	if err != nil {
		return nil, err
	}
	return &TrackRequest{inner: inner}, nil
}

// Requests ranges over subscriber-requested tracks until the dynamic source ends.
func (d *BroadcastDynamic) Requests(ctx context.Context) iter.Seq2[*TrackRequest, error] {
	return streamSeq(ctx, d.RequestedTrack)
}

// Cancel stops the dynamic request stream.
func (d *BroadcastDynamic) Cancel() {
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

// Dynamic creates a fetch handler before accepting this requested track.
func (r *TrackRequest) Dynamic() (*TrackDynamic, error) {
	inner, err := r.inner.Dynamic()
	if err != nil {
		return nil, err
	}
	return &TrackDynamic{inner: inner}, nil
}

// Accept accepts the request as a raw track. For media, use PublishMediaOnTrack.
func (r *TrackRequest) Accept(info *TrackInfo) (*TrackProducer, error) {
	inner, err := r.inner.Accept(info)
	if err != nil {
		return nil, err
	}
	return &TrackProducer{inner: inner}, nil
}

// Abort rejects the request with an application error code.
func (r *TrackRequest) Abort(errorCode uint16) error {
	return r.inner.Abort(int32(errorCode))
}

// MediaProducer writes timestamped frames into a media track.
type MediaProducer struct {
	inner *ffi.MoqMediaProducer
}

// Name is the generated media track name.
func (m *MediaProducer) Name() (string, error) {
	return m.inner.Name()
}

// Used blocks until the track has at least one active subscriber. There is no
// underlying cancel, so a cancelled ctx returns ctx.Err() while the wait
// unwinds when the track is finished or dropped.
func (m *MediaProducer) Used(ctx context.Context) error {
	return runErr(ctx, nil, m.inner.Used)
}

// Unused blocks until the track has no active subscribers. See Used regarding
// cancellation.
func (m *MediaProducer) Unused(ctx context.Context) error {
	return runErr(ctx, nil, m.inner.Unused)
}

// WriteFrame appends a frame with a presentation timestamp in microseconds.
func (m *MediaProducer) WriteFrame(payload []byte, timestampUs uint64) error {
	return m.inner.WriteFrame(payload, timestampUs)
}

// Finish closes the media track.
func (m *MediaProducer) Finish() error {
	return m.inner.Finish()
}

// MediaStreamProducer feeds a raw encoder byte stream; whole frames are emitted
// as they complete.
type MediaStreamProducer struct {
	inner *ffi.MoqMediaStreamProducer
}

// Write pushes raw stream bytes.
func (m *MediaStreamProducer) Write(payload []byte) error {
	return m.inner.Write(payload)
}

// Finish closes the stream.
func (m *MediaStreamProducer) Finish() error {
	return m.inner.Finish()
}

// TrackProducer writes arbitrary byte payloads with no codec required.
type TrackProducer struct {
	inner *ffi.MoqTrackProducer
}

// Name is the track name.
func (t *TrackProducer) Name() (string, error) {
	return t.inner.Name()
}

// Used blocks until the track has at least one active subscriber. See
// MediaProducer.Used regarding cancellation.
func (t *TrackProducer) Used(ctx context.Context) error {
	return runErr(ctx, nil, t.inner.Used)
}

// Unused blocks until the track has no active subscribers. See
// MediaProducer.Used regarding cancellation.
func (t *TrackProducer) Unused(ctx context.Context) error {
	return runErr(ctx, nil, t.inner.Unused)
}

// Dynamic serves fetches for groups that are not currently cached.
func (t *TrackProducer) Dynamic() (*TrackDynamic, error) {
	inner, err := t.inner.Dynamic()
	if err != nil {
		return nil, err
	}
	return &TrackDynamic{inner: inner}, nil
}

// AppendGroup starts a new group; write frames into it, then Finish.
func (t *TrackProducer) AppendGroup() (*GroupProducer, error) {
	inner, err := t.inner.AppendGroup()
	if err != nil {
		return nil, err
	}
	return &GroupProducer{inner: inner}, nil
}

// WriteFrame writes a single-frame group with a timestamp in microseconds.
func (t *TrackProducer) WriteFrame(payload []byte, timestampUs uint64) error {
	return t.inner.WriteFrame(payload, timestampUs)
}

// AppendDatagram sends a best-effort datagram and returns its sequence number.
// timestampUs is the presentation timestamp in microseconds. Payloads are capped
// at 1200 bytes. There is no stream fallback.
func (t *TrackProducer) AppendDatagram(timestampUs uint64, payload []byte) (uint64, error) {
	return t.inner.AppendDatagram(timestampUs, payload)
}

// Abort closes the track with an application error code.
func (t *TrackProducer) Abort(errorCode uint16) error {
	return t.inner.Abort(int32(errorCode))
}

// Consume reads directly from this producer's track. subscription tunes delivery
// (delivery priority, group ordering priority, group range); pass nil for defaults.
func (t *TrackProducer) Consume(subscription *Subscription) (*TrackConsumer, error) {
	inner, err := t.inner.Consume(subscription)
	if err != nil {
		return nil, err
	}
	return &TrackConsumer{inner: inner}, nil
}

// Finish closes the track.
func (t *TrackProducer) Finish() error {
	return t.inner.Finish()
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

// WriteFrame appends a frame with a timestamp in microseconds.
func (g *GroupProducer) WriteFrame(payload []byte, timestampUs uint64) error {
	return g.inner.WriteFrame(payload, timestampUs)
}

// Finish closes the group.
func (g *GroupProducer) Finish() error {
	return g.inner.Finish()
}

// TrackDynamic yields uncached groups requested by fetch consumers.
type TrackDynamic struct {
	inner *ffi.MoqTrackDynamic
}

// RequestedGroup waits for the next uncached group request.
func (d *TrackDynamic) RequestedGroup(ctx context.Context) (*GroupRequest, error) {
	inner, err := runCancellable(ctx, d.inner.Cancel, d.inner.RequestedGroup)
	if err != nil {
		return nil, err
	}
	return &GroupRequest{inner: inner}, nil
}

// Cancel stops current and future requested-group waits.
func (d *TrackDynamic) Cancel() {
	d.inner.Cancel()
}

// GroupRequest requests one uncached group from a track producer.
type GroupRequest struct {
	inner *ffi.MoqGroupRequest
}

// Sequence is the requested group sequence within the track.
func (r *GroupRequest) Sequence() uint64 {
	return r.inner.Sequence()
}

// Priority is the consumer's delivery priority for this fetch.
func (r *GroupRequest) Priority() uint8 {
	return r.inner.Priority()
}

// Accept accepts the request and returns a producer for the group.
func (r *GroupRequest) Accept() (*GroupProducer, error) {
	inner, err := r.inner.Accept()
	if err != nil {
		return nil, err
	}
	return &GroupProducer{inner: inner}, nil
}

// Abort rejects the fetch with an application error code.
func (r *GroupRequest) Abort(errorCode int32) error {
	return r.inner.Abort(errorCode)
}

// AudioProducer pushes raw PCM and lets libopus encode it on the way out.
type AudioProducer struct {
	inner *ffi.MoqAudioProducer
}

// Write pushes one frame of PCM in the configured input format.
func (a *AudioProducer) Write(frame AudioFrame) error {
	return a.inner.Write(frame)
}

// Finish flushes pending samples and finalizes the track.
func (a *AudioProducer) Finish() error {
	return a.inner.Finish()
}
