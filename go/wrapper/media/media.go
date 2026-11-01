// Package media owns catalogs, encoded media imports, and container consumers.
package media

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"iter"
	"time"

	"moq.dev/moq"
	ffi "moq.dev/moq-ffi/moq"
	"moq.dev/moq/internal/bridge"
)

func init() {
	bridge.CatalogProducer = func(v any) *ffi.MoqMediaCatalogProducer { return v.(*CatalogProducer).inner }
}

type (
	// Audio describes one audio rendition in a broadcast catalog: codec, sample rate, channel count, and container.
	Audio = ffi.MoqAudio
	// Catalog is a broadcast's manifest: its video and audio renditions plus display metadata.
	Catalog = ffi.MoqCatalog
	// Dimensions is a width and height in pixels.
	Dimensions = ffi.MoqDimensions
	// DecodeStart is where an audio or video decoder starts on a track that already holds groups.
	DecodeStart = ffi.MoqDecodeStart
	// Video describes one catalog rendition, including whether the publisher recommends temporarily avoiding it.
	Video = ffi.MoqVideo
	// VideoHint supplies catalog fields a video stream can't reveal itself, such as bitrate, filling only the gaps.
	VideoHint = ffi.MoqVideoHint
	// AudioFormat is a single audio codec an importer can parse.
	AudioFormat = ffi.MoqAudioFormat
	// VideoFormat is a single video codec an importer can parse.
	VideoFormat = ffi.MoqVideoFormat
	// ContainerFormat is a container that publishes its own tracks.
	ContainerFormat = ffi.MoqContainerFormat
	// VideoProperties holds catalog properties shared by every video rendition; nil fields clear those properties.
	VideoProperties = ffi.MoqVideoProperties
	// Container selects how subscribed media frames are demuxed. Build one with
	// LegacyContainer, CmafContainer, or LocContainer.
	Container = ffi.MoqContainer
	// ContainerLegacy is the legacy hang container variant of Container; build one with LegacyContainer.
	ContainerLegacy = ffi.MoqContainerLegacy
	// ContainerCmaf is the CMAF (fMP4) container variant of Container, carrying its init segment; build one with CmafContainer.
	ContainerCmaf = ffi.MoqContainerCmaf
	// ContainerLoc is the low-overhead container variant of Container; build one with LocContainer.
	ContainerLoc = ffi.MoqContainerLoc
	// Rendition is a catalog rendition: its ID (catalog key) and config. Build one with VideoRendition or AudioRendition.
	Rendition = ffi.MoqRendition
	// RenditionVideo is the video variant of Rendition; build one with VideoRendition.
	RenditionVideo = ffi.MoqRenditionVideo
	// RenditionAudio is the audio variant of Rendition; build one with AudioRendition.
	RenditionAudio = ffi.MoqRenditionAudio

	// AudioInit describes an audio codec and its initialization bytes.
	AudioInit = ffi.MoqAudioInit
	// VideoInit describes a video codec, initialization bytes, and hints.
	VideoInit = ffi.MoqVideoInit
)

// Timebase shares a source's timestamp offset without keeping its broadcast open.
type Timebase struct{ inner *ffi.MoqMediaTimebase }

// NewTimebase creates a shared source timebase owned by broadcast.
func NewTimebase(broadcast *moq.BroadcastProducer) (*Timebase, error) {
	inner, err := ffi.NewMoqMediaTimebase(bridge.BroadcastProducer(broadcast))
	if err != nil {
		return nil, err
	}
	return &Timebase{inner: inner}, nil
}

// Close releases this timebase handle on the calling goroutine; existing imports keep their offset.
func (t *Timebase) Close() { t.inner.Destroy() }

// ContainerInit describes a container, its leading bytes and an optional shared timebase.
type ContainerInit struct {
	// Format selects the container parser.
	Format ContainerFormat
	// Data supplies the leading container bytes.
	Data []byte
	// Timebase shares a source offset, or nil for an independent offset.
	Timebase *Timebase
}

// MediaFrame is a container-decoded payload, presentation timestamp, and keyframe flag.
type MediaFrame struct {
	Payload   []byte
	Timestamp time.Duration
	Keyframe  bool
}

// Target is the named or requested track an importer publishes.
type Target interface{ target() }

// Named publishes a new track with a chosen name, or a format-derived unique name.
type Named struct{ Name *string }

func (Named) target() {}

// Requested publishes a subscriber's pending track request.
type Requested struct{ Request *moq.TrackRequest }

func (Requested) target() {}

func targetFFI(target Target) (ffi.MoqMediaTarget, error) {
	switch target := target.(type) {
	case Named:
		return ffi.MoqMediaTargetNamed{Name: target.Name}, nil
	case Requested:
		if target.Request != nil {
			return ffi.MoqMediaTargetRequested{Request: bridge.TrackRequest(target.Request)}, nil
		}
	case *Named:
		if target != nil {
			return targetFFI(*target)
		}
	case *Requested:
		if target != nil {
			return targetFFI(*target)
		}
	}
	return nil, errors.New("media: a named target or a non-nil requested track is required")
}

// CatalogProducer updates a catalog without keeping its broadcast open.
type CatalogProducer struct{ inner *ffi.MoqMediaCatalogProducer }

// NewCatalogProducer creates a weak catalog handle for broadcast.
func NewCatalogProducer(broadcast *moq.BroadcastProducer) (*CatalogProducer, error) {
	inner, err := ffi.NewMoqMediaCatalogProducer(bridge.BroadcastProducer(broadcast))
	if err != nil {
		return nil, err
	}
	return &CatalogProducer{inner: inner}, nil
}

// Now reads the broadcast clock, fixing it for subsequent media producers.
func (c *CatalogProducer) Now() (time.Duration, error) {
	us, err := c.inner.Now()
	return time.Duration(us) * time.Microsecond, err
}

// SetVideoProperties replaces the video properties shared by all renditions.
func (c *CatalogProducer) SetVideoProperties(properties VideoProperties) error {
	return c.inner.SetVideoProperties(properties)
}

// SetSection publishes an application catalog section from a JSON-serializable value.
func (c *CatalogProducer) SetSection(name string, value any) error {
	data, err := json.Marshal(value)
	if err != nil {
		return err
	}
	return c.inner.SetSection(name, string(data))
}

// RemoveSection removes an application catalog section if present.
func (c *CatalogProducer) RemoveSection(name string) error { return c.inner.RemoveSection(name) }

// Finish finishes the catalog, which ends the broadcast's content.
//
// Finish every track first. The broadcast stays announced and keeps serving its
// finished tracks to late subscribers until [moq.BroadcastProducer.Close] takes it
// offline; how long to keep it up is yours to decide.
func (c *CatalogProducer) Finish() error { return c.inner.Finish() }

// NewAudioTrackProducer imports encoded audio frames on target.
func NewAudioTrackProducer(broadcast *moq.BroadcastProducer, target Target, init AudioInit) (*TrackProducer, error) {
	resolved, err := targetFFI(target)
	if err != nil {
		return nil, err
	}
	inner, err := ffi.MoqMediaTrackProducerAudio(bridge.BroadcastProducer(broadcast), resolved, init)
	if err != nil {
		return nil, err
	}
	return &TrackProducer{inner: inner}, nil
}

// NewVideoTrackProducer imports encoded video frames on target.
func NewVideoTrackProducer(broadcast *moq.BroadcastProducer, target Target, init VideoInit) (*TrackProducer, error) {
	resolved, err := targetFFI(target)
	if err != nil {
		return nil, err
	}
	inner, err := ffi.MoqMediaTrackProducerVideo(bridge.BroadcastProducer(broadcast), resolved, init)
	if err != nil {
		return nil, err
	}
	return &TrackProducer{inner: inner}, nil
}

// NewVideoTrackStreamProducer imports a video byte stream on target.
func NewVideoTrackStreamProducer(broadcast *moq.BroadcastProducer, target Target, init VideoInit) (*TrackStreamProducer, error) {
	resolved, err := targetFFI(target)
	if err != nil {
		return nil, err
	}
	inner, err := ffi.MoqMediaTrackStreamProducerVideo(bridge.BroadcastProducer(broadcast), resolved, init)
	if err != nil {
		return nil, err
	}
	return &TrackStreamProducer{inner: inner}, nil
}

// NewContainerProducer imports a container from chunks split anywhere, such as a byte stream.
func NewContainerProducer(broadcast *moq.BroadcastProducer, init ContainerInit) (*ContainerProducer, error) {
	var timebase **ffi.MoqMediaTimebase
	if init.Timebase != nil {
		timebase = &init.Timebase.inner
	}
	inner, err := ffi.NewMoqMediaContainerProducer(bridge.BroadcastProducer(broadcast), ffi.MoqContainerInit{Format: init.Format, Data: init.Data, Timebase: timebase})
	if err != nil {
		return nil, err
	}
	return &ContainerProducer{inner: inner}, nil
}

// NewCatalogConsumer subscribes to broadcast's catalog snapshots.
func NewCatalogConsumer(ctx context.Context, broadcast *moq.BroadcastConsumer) (*CatalogConsumer, error) {
	inner, err := ffi.MoqMediaCatalogConsumerSubscribe(ctx, bridge.BroadcastConsumer(broadcast))
	if err != nil {
		return nil, err
	}
	return &CatalogConsumer{inner: inner}, nil
}

// TrackConfig names a catalog rendition and live delivery options.
type TrackConfig struct {
	Rendition    Rendition
	Subscription *moq.Subscription
}

// NewTrackConsumer subscribes to and decodes a catalog rendition, following its broadcast and track.
func NewTrackConsumer(ctx context.Context, broadcast *moq.BroadcastConsumer, config TrackConfig) (*TrackConsumer, error) {
	if err := bridge.Rendition(config.Rendition); err != nil {
		return nil, err
	}
	sub, err := bridge.Subscription(config.Subscription)
	if err != nil {
		return nil, err
	}
	inner, err := ffi.MoqMediaTrackConsumerSubscribe(ctx, bridge.BroadcastConsumer(broadcast), ffi.MoqMediaTrackConfig{Rendition: config.Rendition, Subscription: sub})
	if err != nil {
		return nil, err
	}
	return &TrackConsumer{inner: inner}, nil
}

// GroupConfig names a catalog rendition's group and fetch delivery options.
type GroupConfig struct {
	Rendition Rendition
	Sequence  uint64
	Options   *moq.FetchGroupOptions
}

// FetchGroup fetches and decodes exactly one group of a catalog rendition, following its broadcast and track.
func FetchGroup(ctx context.Context, broadcast *moq.BroadcastConsumer, config GroupConfig) (*GroupConsumer, error) {
	if err := bridge.Rendition(config.Rendition); err != nil {
		return nil, err
	}
	if config.Options != nil {
		if err := bridge.NonNegative(config.Options.MaxDelay, "max delay"); err != nil {
			return nil, err
		}
	}
	inner, err := ffi.MoqMediaGroupConsumerFetch(ctx, bridge.BroadcastConsumer(broadcast), ffi.MoqMediaGroupConfig{Rendition: config.Rendition, Sequence: config.Sequence, Options: config.Options})
	if err != nil {
		return nil, err
	}
	return &GroupConsumer{inner: inner}, nil
}

// NewGroupConsumer decodes a raw group with the track's container, as each frame lands.
//
// Pair it with moq.TrackConsumer.RecvGroup to read media in arrival order: frame timestamps are
// container media time, not the transport's. Which groups arrive, and how long a stalled one
// stays readable, is the subscription's MaxDelay. Takes over group, whose handle is closed
// afterward; errors if it has already read a frame.
func NewGroupConsumer(group *moq.GroupConsumer, container Container) (*GroupConsumer, error) {
	inner, err := ffi.NewMoqMediaGroupConsumer(bridge.GroupConsumer(group), container)
	if err != nil {
		return nil, err
	}
	return &GroupConsumer{inner: inner}, nil
}

// VideoRendition selects the video rendition the catalog lists under id.
func VideoRendition(id string, config Video) Rendition {
	return RenditionVideo{Id: id, Config: config}
}

// AudioRendition selects the audio rendition the catalog lists under id.
func AudioRendition(id string, config Audio) Rendition {
	return RenditionAudio{Id: id, Config: config}
}

// LegacyContainer selects the legacy hang container for a media subscription.
func LegacyContainer() Container {
	return ContainerLegacy{}
}

// CmafContainer selects the CMAF (fMP4) container for a media subscription,
// initialized from the given init segment.
func CmafContainer(init []byte) Container {
	return ContainerCmaf{Init: init}
}

// LocContainer selects the low-overhead container for a media subscription.
func LocContainer() Container {
	return ContainerLoc{}
}

// AudioFormat values: a single audio codec an importer can parse.
const (
	// AudioFormatAac is Advanced Audio Coding, configured by an AudioSpecificConfig.
	AudioFormatAac = ffi.MoqAudioFormatAac
	// AudioFormatOpus is Opus, configured by an OpusHead.
	AudioFormatOpus = ffi.MoqAudioFormatOpus
	// AudioFormatFlac is FLAC, configured by its STREAMINFO block.
	AudioFormatFlac = ffi.MoqAudioFormatFlac
	// AudioFormatMp3 is MPEG-1/2 Audio Layer III.
	AudioFormatMp3 = ffi.MoqAudioFormatMp3
)

// VideoFormat values: a single video codec an importer can parse. The two H.26x pairs
// differ by framing, not codec: Avc1/Hvc1 are length-prefixed with an out-of-band config
// record, Avc3/Hev1 are Annex-B with the parameter sets inline.
const (
	// VideoFormatAvc1 is H.264 with length-prefixed NALUs and an out-of-band avcC.
	VideoFormatAvc1 = ffi.MoqVideoFormatAvc1
	// VideoFormatAvc3 is H.264 in Annex-B with inline SPS/PPS.
	VideoFormatAvc3 = ffi.MoqVideoFormatAvc3
	// VideoFormatHvc1 is H.265 with length-prefixed NALUs and an out-of-band hvcC.
	VideoFormatHvc1 = ffi.MoqVideoFormatHvc1
	// VideoFormatHev1 is H.265 in Annex-B with inline parameter sets.
	VideoFormatHev1 = ffi.MoqVideoFormatHev1
	// VideoFormatAv01 is AV1.
	VideoFormatAv01 = ffi.MoqVideoFormatAv01
	// VideoFormatVp8 is VP8.
	VideoFormatVp8 = ffi.MoqVideoFormatVp8
	// VideoFormatVp9 is VP9.
	VideoFormatVp9 = ffi.MoqVideoFormatVp9
)

// ContainerFormat values: a container that publishes its own tracks.
const (
	// ContainerFormatFmp4 is fragmented MP4 / CMAF, split into one frame per sample.
	ContainerFormatFmp4 = ffi.MoqContainerFormatFmp4
	// ContainerFormatMkv is Matroska / WebM.
	ContainerFormatMkv = ffi.MoqContainerFormatMkv
	// ContainerFormatTs is an MPEG-2 transport stream.
	ContainerFormatTs = ffi.MoqContainerFormatTs
	// ContainerFormatFlv is Flash Video, as used by RTMP.
	ContainerFormatFlv = ffi.MoqContainerFormatFlv
)

// DecodeStart values. A decoder's zero value starts at the oldest group.
const (
	// DecodeStartOldest decodes everything the track still holds: what a recorder, an export, or anything reading a finished track wants.
	DecodeStartOldest = ffi.MoqDecodeStartOldest
	// DecodeStartLatest joins at the newest group and skips the cached backlog: what a live player wants, so reopening a decoder does not replay what it already played.
	DecodeStartLatest = ffi.MoqDecodeStartLatest
)

// TrackProducer writes timestamped frames into a media track.
type TrackProducer struct {
	inner *ffi.MoqMediaTrackProducer
}

// Demand returns a watch-only handle to whether the track has subscribers.
func (m *TrackProducer) Demand() (*moq.TrackDemand, error) {
	inner, err := m.inner.Demand()
	if err != nil {
		return nil, err
	}
	return bridge.TrackDemand(inner).(*moq.TrackDemand), nil
}

// WriteFrame appends frame to the media track. The importer derives keyframe status from
// the bitstream, so a Frame carries only the payload and its timestamp.
func (m *TrackProducer) WriteFrame(frame moq.Frame) error {
	f, err := bridge.Frame(frame)
	if err != nil {
		return err
	}
	return m.inner.WriteFrame(f)
}

// Discontinuity marks a timeline break, preserving the advertised jitter and delay.
func (m *TrackProducer) Discontinuity() error {
	return m.inner.Discontinuity()
}

// Cut draws a group boundary here.
//
// Audio has no boundary of its own (every packet is independently decodable), so this is
// the only thing that gives it groups: call it after every frame for one group (one QUIC
// stream) the relay forwards without waiting, or at a segment cadence to align with video.
// Video groups at its own keyframes and needs this only to override that.
func (m *TrackProducer) Cut() error {
	return m.inner.Cut()
}

// Seek draws a group boundary and numbers the next group sequence.
//
// Cut with an explicit sequence, for a publisher whose group numbers have to be
// deterministic: two encoders aligning per GOP so a consumer can fail over between them.
func (m *TrackProducer) Seek(sequence uint64) error {
	return m.inner.Seek(sequence)
}

// Finish closes the media track.
func (m *TrackProducer) Finish() error {
	return m.inner.Finish()
}

// ContainerProducer publishes a container, which demuxes and publishes its own
// tracks. Unlike TrackProducer there is no per-frame timestamp: a container
// carries its tracks' timing itself.
type ContainerProducer struct {
	inner *ffi.MoqMediaContainerProducer
}

// Write pushes container bytes; the container recovers its own framing.
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

// TrackStreamProducer feeds a raw encoder byte stream; whole frames are emitted
// as they complete.
type TrackStreamProducer struct {
	inner *ffi.MoqMediaTrackStreamProducer
}

// Demand watches whether this track has subscribers.
func (m *TrackStreamProducer) Demand() (*moq.TrackDemand, error) {
	inner, err := m.inner.Demand()
	if err != nil {
		return nil, err
	}
	return bridge.TrackDemand(inner).(*moq.TrackDemand), nil
}

// Write pushes raw stream bytes.
func (m *TrackStreamProducer) Write(payload []byte) error {
	return m.inner.Write(payload)
}

// Finish closes the stream.
func (m *TrackStreamProducer) Finish() error {
	return m.inner.Finish()
}

// mediaFrame converts the result of a Next call, keeping (nil, nil) for the end.
func mediaFrame(frame *ffi.MoqMediaFrame, err error) (*MediaFrame, error) {
	if frame == nil || err != nil {
		return nil, err
	}
	return &MediaFrame{Payload: frame.Payload, Timestamp: time.Duration(frame.TimestampUs) * time.Microsecond, Keyframe: frame.Keyframe}, nil
}

// Read is one encoded frame, a ReadGap, or a ReadEnd, in delivery order.
type Read interface{ read() }

// ReadFrame is an encoded frame and its container metadata.
type ReadFrame struct{ Frame MediaFrame }

// ReadGap is lost media; what follows does not continue it.
type ReadGap struct {
	Timestamp time.Duration
	Duration  time.Duration
}

// ReadEnd is where a stopped encoder's media stops. Following frames before End
// are its remaining media; the first at or past End comes from a fresh encoder.
type ReadEnd struct{ End time.Duration }

func (ReadFrame) read() {}
func (ReadGap) read()   {}
func (ReadEnd) read()   {}

func mediaRead(raw ffi.MoqMediaRead) (Read, error) {
	switch read := raw.(type) {
	case ffi.MoqMediaReadFrame:
		frame, err := mediaFrame(&read.Frame, nil)
		if err != nil {
			return nil, err
		}
		return ReadFrame{Frame: *frame}, nil
	case ffi.MoqMediaReadGap:
		return ReadGap{Timestamp: time.Duration(read.TimestampUs) * time.Microsecond, Duration: read.Duration}, nil
	case ffi.MoqMediaReadEnd:
		return ReadEnd{End: time.Duration(read.EndUs) * time.Microsecond}, nil
	}
	return nil, fmt.Errorf("unknown media read: %T", raw)
}

// TrackConsumer is a stream of encoded frames, gaps, and encoder ends.
type TrackConsumer struct {
	inner *ffi.MoqMediaTrackConsumer
}

// Next returns the next read, or (nil, nil) once the track ends.
func (m *TrackConsumer) Next(ctx context.Context) (Read, error) {
	raw, err := bridge.Call(ctx, m.inner.Cancel, m.inner.Next)
	if err != nil || raw == nil {
		return nil, err
	}
	return mediaRead(*raw)
}

// All ranges over reads until the track ends or the loop breaks.
func (m *TrackConsumer) All(ctx context.Context) iter.Seq2[Read, error] {
	return bridge.Seq(ctx, m.Next)
}

// Cancel stops the stream.
func (m *TrackConsumer) Cancel() {
	m.inner.Cancel()
}

// GroupConsumer is a finite stream of decoded media frames from one group,
// fetched or decoded from a raw group. It ends after the group's last frame.
type GroupConsumer struct {
	inner *ffi.MoqMediaGroupConsumer
}

// Sequence is this group's sequence number within the track.
func (m *GroupConsumer) Sequence() uint64 {
	return m.inner.Sequence()
}

// Next returns the next decoded frame, or (nil, nil) when the group ends.
func (m *GroupConsumer) Next(ctx context.Context) (*MediaFrame, error) {
	return mediaFrame(bridge.Call(ctx, m.inner.Cancel, m.inner.Next))
}

// All ranges over decoded frames until the group ends or the loop breaks.
func (m *GroupConsumer) All(ctx context.Context) iter.Seq2[*MediaFrame, error] {
	return bridge.Seq(ctx, m.Next)
}

// Cancel stops the stream.
func (m *GroupConsumer) Cancel() {
	m.inner.Cancel()
}

// CatalogConsumer is a stream of catalog updates.
type CatalogConsumer struct {
	inner *ffi.MoqMediaCatalogConsumer
}

// Next returns the next catalog, or (nil, nil) when the track ends.
func (c *CatalogConsumer) Next(ctx context.Context) (*Catalog, error) {
	return bridge.Call(ctx, c.inner.Cancel, c.inner.Next)
}

// All ranges over catalog updates until the track ends or the loop breaks.
func (c *CatalogConsumer) All(ctx context.Context) iter.Seq2[*Catalog, error] {
	return bridge.Seq(ctx, c.Next)
}

// Cancel stops the stream.
func (c *CatalogConsumer) Cancel() {
	c.inner.Cancel()
}

// CatalogSnapshot reads the first catalog snapshot and releases its subscription.
func CatalogSnapshot(ctx context.Context, broadcast *moq.BroadcastConsumer) (*Catalog, error) {
	consumer, err := NewCatalogConsumer(ctx, broadcast)
	if err != nil {
		return nil, err
	}
	defer consumer.Cancel()
	snapshot, err := consumer.Next(ctx)
	if err == nil && snapshot == nil {
		err = moq.ErrClosed
	}
	return snapshot, err
}
