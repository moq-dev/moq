package moq

import (
	"context"
	"errors"
	"iter"

	ffi "moq.dev/moq-ffi/moq"
)

// AudioOption configures an audio publish.
type AudioOption func(*ffi.MoqAudioInit)

// VideoOption configures a video publish.
type VideoOption func(*ffi.MoqVideoInit)

var errNilTrackRequest = errors.New("moq: nil track request")

// WithAudioLabel sets the human-readable rendition name stored in the catalog.
func WithAudioLabel(label string) AudioOption {
	return func(init *ffi.MoqAudioInit) {
		init.Label = &label
	}
}

// WithVideoLabel sets the human-readable rendition name stored in the catalog.
func WithVideoLabel(label string) VideoOption {
	return func(init *ffi.MoqVideoInit) {
		init.Label = &label
	}
}

// WithVideoHint seeds catalog fields that a video stream cannot reveal itself.
func WithVideoHint(hint VideoHint) VideoOption {
	return func(init *ffi.MoqVideoInit) {
		init.Hint = &hint
	}
}

func audioInit(format AudioFormat, init []byte, opts []AudioOption) ffi.MoqAudioInit {
	cfg := ffi.MoqAudioInit{Format: format, Data: init}
	for _, opt := range opts {
		opt(&cfg)
	}
	return cfg
}

func videoInit(format VideoFormat, init []byte, opts []VideoOption) ffi.MoqVideoInit {
	cfg := ffi.MoqVideoInit{Format: format, Data: init}
	for _, opt := range opts {
		opt(&cfg)
	}
	return cfg
}

// BroadcastProducer publishes a collection of tracks. Create one at a path with
// [OriginProducer.CreateBroadcast] (or [Client.CreateBroadcast]), then publish
// tracks onto it.
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

// Dynamic accepts requests for tracks that are not published yet.
func (b *BroadcastProducer) Dynamic() (*BroadcastDynamic, error) {
	inner, err := b.inner.Dynamic()
	if err != nil {
		return nil, err
	}
	return &BroadcastDynamic{inner: inner}, nil
}

// SetRoute sets the broadcast's route: the hop chain, cost, and liveness it
// advertises. Use it as conditions shift (e.g. a standby transcoder lowering
// its cost once warm); consumers observe the change via RouteUpdates.
func (b *BroadcastProducer) SetRoute(route Route) error {
	return b.inner.SetRoute(route)
}

// SetAnnounce sets whether the broadcast is announced, keeping the rest of its route.
//
// The origin announces the path only while the broadcast is announced; a unannounced
// broadcast stays reachable by exact path for subscribes and fetches. This is
// how a publisher goes on and off the air without tearing down the broadcast.
func (b *BroadcastProducer) SetAnnounce(live bool) error {
	return b.inner.SetAnnounce(live)
}

// SetVideoProperties replaces the catalog properties shared by every video rendition.
func (b *BroadcastProducer) SetVideoProperties(properties VideoProperties) error {
	return b.inner.SetVideoProperties(properties)
}

// PublishAudio publishes one audio codec as a new track, fed frame by frame with
// explicit timestamps. init carries the codec init bytes, which audio requires.
func (b *BroadcastProducer) PublishAudio(format AudioFormat, init []byte, opts ...AudioOption) (*MediaProducer, error) {
	inner, err := b.inner.PublishAudio(audioInit(format, init, opts))
	if err != nil {
		return nil, err
	}
	return &MediaProducer{inner: inner}, nil
}

// PublishVideo publishes one video codec as a new track. init may be nil for a
// format that resolves in band.
func (b *BroadcastProducer) PublishVideo(format VideoFormat, init []byte, opts ...VideoOption) (*MediaProducer, error) {
	inner, err := b.inner.PublishVideo(videoInit(format, init, opts))
	if err != nil {
		return nil, err
	}
	return &MediaProducer{inner: inner}, nil
}

// PublishContainer publishes a container, which demuxes and publishes its own
// tracks. There is no label or hint: a container describes each track itself.
func (b *BroadcastProducer) PublishContainer(format ContainerFormat, init []byte) (*ContainerProducer, error) {
	inner, err := b.inner.PublishContainer(ffi.MoqContainerInit{Format: format, Data: init})
	if err != nil {
		return nil, err
	}
	return &ContainerProducer{inner: inner}, nil
}

// PublishAudioOnTrack publishes one audio codec onto a subscriber-requested track.
func (b *BroadcastProducer) PublishAudioOnTrack(request *TrackRequest, format AudioFormat, init []byte, opts ...AudioOption) (*MediaProducer, error) {
	if request == nil {
		return nil, errNilTrackRequest
	}
	inner, err := b.inner.PublishAudioOnTrack(request.inner, audioInit(format, init, opts))
	if err != nil {
		return nil, err
	}
	return &MediaProducer{inner: inner}, nil
}

// PublishVideoOnTrack publishes one video codec onto a subscriber-requested track.
func (b *BroadcastProducer) PublishVideoOnTrack(request *TrackRequest, format VideoFormat, init []byte, opts ...VideoOption) (*MediaProducer, error) {
	if request == nil {
		return nil, errNilTrackRequest
	}
	inner, err := b.inner.PublishVideoOnTrack(request.inner, videoInit(format, init, opts))
	if err != nil {
		return nil, err
	}
	return &MediaProducer{inner: inner}, nil
}

// PublishVideoStream publishes a video track fed by a raw byte stream with
// unknown frame boundaries (e.g. Annex-B H.264). Only the self-delimiting
// formats work: Avc3, Hev1, Av01. Audio has no counterpart, having no frame
// boundaries to infer.
func (b *BroadcastProducer) PublishVideoStream(format VideoFormat, opts ...VideoOption) (*MediaStreamProducer, error) {
	inner, err := b.inner.PublishVideoStream(videoInit(format, nil, opts))
	if err != nil {
		return nil, err
	}
	return &MediaStreamProducer{inner: inner}, nil
}

// PublishContainerStream publishes a container fed by a raw byte stream, which
// recovers its own framing.
func (b *BroadcastProducer) PublishContainerStream(format ContainerFormat) (*ContainerStreamProducer, error) {
	inner, err := b.inner.PublishContainerStream(format)
	if err != nil {
		return nil, err
	}
	return &ContainerStreamProducer{inner: inner}, nil
}

// EncodeAudio publishes a raw-audio track with an in-process Opus encoder.
func (b *BroadcastProducer) EncodeAudio(name string, input AudioEncoderInput, output AudioEncoderOutput) (*AudioProducer, error) {
	inner, err := b.inner.EncodeAudio(name, input, output)
	if err != nil {
		return nil, err
	}
	return &AudioProducer{inner: inner}, nil
}

// EncodeVideo publishes a raw-video track with an in-process H.264/H.265
// encoder.
//
// The track is named after the codec (.avc3 / .hev1) and its catalog rendition
// is published immediately, read out of the encoder itself, so subscribers
// discover it through the catalog rather than a name you pick, and can find it
// before the first frame exists.
func (b *BroadcastProducer) EncodeVideo(input VideoEncoderInput, output VideoEncoderOutput) (*VideoProducer, error) {
	inner, err := b.inner.EncodeVideo(input, output)
	if err != nil {
		return nil, err
	}
	return &VideoProducer{inner: inner}, nil
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

// Accept accepts the request as a raw track. For media, use PublishAudioOnTrack or PublishVideoOnTrack.
func (r *TrackRequest) Accept(info *TrackInfo) (*TrackProducer, error) {
	inner, err := r.inner.Accept(info)
	if err != nil {
		return nil, err
	}
	return &TrackProducer{inner: inner}, nil
}

// Abort rejects the request with an application error code.
func (r *TrackRequest) Abort(errorCode uint16) error {
	return r.inner.Abort(errorCode)
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

// WriteFrame appends frame to the media track. The importer derives keyframe status from
// the bitstream, so a Frame carries only the payload and its timestamp.
func (m *MediaProducer) WriteFrame(frame Frame) error {
	return m.inner.WriteFrame(frame)
}

// Cut draws a group boundary here.
//
// Audio has no boundary of its own (every packet is independently decodable), so this is
// the only thing that gives it groups: call it after every frame for one group (one QUIC
// stream) the relay forwards without waiting, or at a segment cadence to align with video.
// Video groups at its own keyframes and needs this only to override that.
//
// On a container this declares a new segment, rolling a group on every track it publishes.
func (m *MediaProducer) Cut() error {
	return m.inner.Cut()
}

// Seek draws a group boundary and numbers the next group sequence.
//
// Cut with an explicit sequence, for a publisher whose group numbers have to be
// deterministic: two encoders aligning per GOP so a consumer can fail over between them.
func (m *MediaProducer) Seek(sequence uint64) error {
	return m.inner.Seek(sequence)
}

// Finish closes the media track.
func (m *MediaProducer) Finish() error {
	return m.inner.Finish()
}

// ContainerProducer publishes a container, which demuxes and publishes its own
// tracks. Unlike MediaProducer there is no per-frame timestamp: a container
// carries its tracks' timing itself.
type ContainerProducer struct {
	inner *ffi.MoqContainerProducer
}

// Write pushes a whole chunk of container bytes.
func (c *ContainerProducer) Write(payload []byte) error {
	return c.inner.Write(payload)
}

// Cut declares that the next chunk starts a new segment, rolling a group on
// every track. An fMP4 source carrying styp atoms declares its own, so this is
// only needed when it doesn't; MKV, TS, and FLV ignore it.
func (c *ContainerProducer) Cut() error {
	return c.inner.Cut()
}

// Seek starts a new segment and numbers its groups sequence.
func (c *ContainerProducer) Seek(sequence uint64) error {
	return c.inner.Seek(sequence)
}

// Finish finishes every track this container publishes.
func (c *ContainerProducer) Finish() error {
	return c.inner.Finish()
}

// ContainerStreamProducer publishes a container fed by a raw byte stream, which
// recovers its own framing.
type ContainerStreamProducer struct {
	inner *ffi.MoqContainerStreamProducer
}

// Write pushes raw container bytes; chunk boundaries don't matter.
func (c *ContainerStreamProducer) Write(payload []byte) error {
	return c.inner.Write(payload)
}

// Finish finishes every track this container publishes.
func (c *ContainerStreamProducer) Finish() error {
	return c.inner.Finish()
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
	return t.inner.WriteFrame(frame)
}

// AppendDatagram sends frame as a best-effort datagram and returns the sequence number
// assigned to it. Payloads are capped at 1200 bytes. There is no stream fallback.
func (t *TrackProducer) AppendDatagram(frame Frame) (uint64, error) {
	return t.inner.AppendDatagram(frame)
}

// Abort closes the track with an application error code.
func (t *TrackProducer) Abort(errorCode uint16) error {
	return t.inner.Abort(errorCode)
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
	return g.inner.WriteFrame(frame)
}

// Finish closes the group.
func (g *GroupProducer) Finish() error {
	return g.inner.Finish()
}

// Abort terminates the group with an application error code.
func (g *GroupProducer) Abort(errorCode uint16) error {
	return g.inner.Abort(errorCode)
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

// Requests ranges over uncached group requests until the dynamic source ends.
func (d *TrackDynamic) Requests(ctx context.Context) iter.Seq2[*GroupRequest, error] {
	return streamSeq(ctx, d.RequestedGroup)
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
func (r *GroupRequest) Abort(errorCode uint16) error {
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

// VideoProducer pushes raw pictures and lets a native encoder compress them on
// the way out.
type VideoProducer struct {
	inner *ffi.MoqVideoProducer
}

// Write encodes and publishes one frame in the configured input format. A
// hardware encoder pipelines, so a call that puts nothing on the wire is normal
// rather than an error.
func (v *VideoProducer) Write(frame VideoFrame) error {
	return v.inner.Write(frame)
}

// Cut starts a new group at the next written frame.
//
// Optional: the encoder keyframes every Gop frames on its own, and each of
// those cuts a group, so a subscriber can always join without this. Reach for it
// only to place the boundaries yourself, aligning groups with something the
// encoder can't see such as a scene change.
func (v *VideoProducer) Cut() error {
	return v.inner.Cut()
}

// SetBitrate retunes the live encoder, in bits per second. Cheap enough to drive
// from a congestion controller: no keyframe is forced. An error means this
// backend can't retune while running, which is not fatal; the encoder keeps its
// current rate.
func (v *VideoProducer) SetBitrate(bitrate uint64) error {
	return v.inner.SetBitrate(bitrate)
}

// Finish flushes any frames the codec is holding and finalizes the track.
func (v *VideoProducer) Finish() error {
	return v.inner.Finish()
}
