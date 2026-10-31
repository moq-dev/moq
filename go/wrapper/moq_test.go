package moq_test

import (
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"runtime"
	"slices"
	"sync"
	"testing"
	"time"

	"moq.dev/moq"
	moqaudio "moq.dev/moq/audio"
	moqmedia "moq.dev/moq/media"
	moqvideo "moq.dev/moq/video"
)

// testTimeout bounds the blocking stream calls so a regression fails the test
// job instead of hanging it.
const testTimeout = 10 * time.Second

// newOrigin returns an origin that lasts the whole test. An OriginProducer has
// no Close: the collector ends its origin once nothing reaches an owner, even
// while consumers made from it are still in use.
func newOrigin(t *testing.T) *moq.OriginProducer {
	origin := moq.NewOriginProducer()
	t.Cleanup(func() { runtime.KeepAlive(origin) })
	return origin
}

// opusHead builds a valid OpusHead init buffer (RFC 7845): 48 kHz, 2 channels.
func opusHead() []byte {
	buf := []byte("OpusHead")
	buf = append(buf, 1, 2) // version, channels
	buf = binary.LittleEndian.AppendUint16(buf, 0)
	buf = binary.LittleEndian.AppendUint32(buf, 48000)
	buf = binary.LittleEndian.AppendUint16(buf, 0)
	buf = append(buf, 0) // channel mapping
	return buf
}

func TestDynamicBroadcastRequest(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	origin := newOrigin(t)
	dynamic, err := origin.Requests("")
	if err != nil {
		t.Fatal(err)
	}
	defer dynamic.Cancel()

	type result struct {
		broadcast *moq.BroadcastConsumer
		err       error
	}
	requested := make(chan result, 1)
	go func() {
		broadcast, err := origin.Consume().Request(ctx, "dynamic/broadcast", nil)
		requested <- result{broadcast: broadcast, err: err}
	}()

	request, err := dynamic.RequestedBroadcast(ctx)
	if err != nil {
		t.Fatal(err)
	}
	path, err := request.Path()
	if err != nil {
		t.Fatal(err)
	}
	if path != "dynamic/broadcast" {
		t.Fatalf("path = %q, want %q", path, "dynamic/broadcast")
	}

	prepared, err := request.Prepare(moq.MintEpoch())
	if err != nil {
		t.Fatal(err)
	}
	track, err := prepared.Broadcast().PublishTrack("status", timedInfo())
	if err != nil {
		t.Fatal(err)
	}
	served, err := prepared.Publish()
	if err != nil {
		t.Fatal(err)
	}

	var res result
	select {
	case res = <-requested:
	case <-ctx.Done():
		t.Fatal(ctx.Err())
	}
	if res.err != nil {
		t.Fatal(res.err)
	}

	trackConsumer, err := res.broadcast.SubscribeTrack(ctx, "status", nil)
	if err != nil {
		t.Fatal(err)
	}
	defer trackConsumer.Cancel()

	payload := []byte("served dynamically")
	if err := track.WriteFrame(moq.Frame{Payload: payload, Timestamp: ts(0)}); err != nil {
		t.Fatal(err)
	}
	frame, err := trackConsumer.ReadFrame(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if frame == nil || string(frame.Payload) != string(payload) || frame.Timestamp == nil || *frame.Timestamp != 0 {
		t.Fatalf("frame = %+v, want payload=%q ts=0", frame, payload)
	}

	if err := track.Finish(); err != nil {
		t.Fatal(err)
	}
	if err := served.Broadcast().Close(); err != nil {
		t.Fatal(err)
	}
	// The publication fills the prepared broadcast, so closing one closes the other.
	if _, err := prepared.Broadcast().PublishTrack("late", nil); !errors.Is(err, moq.ErrClosed) {
		t.Fatalf("PublishTrack after close = %v, want ErrClosed", err)
	}
}

func TestPublishAudioLifecycle(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	media, err := moqmedia.NewAudioTrackProducer(broadcast, moqmedia.Named{}, moqmedia.AudioInit{Format: moqmedia.AudioFormatOpus, Data: opusHead()})
	if err != nil {
		t.Fatal(err)
	}
	if err := media.WriteFrame(moq.Frame{Payload: []byte("opus frame"), Timestamp: ts(1000 * time.Microsecond)}); err != nil {
		t.Fatal(err)
	}
	if err := media.Finish(); err != nil {
		t.Fatal(err)
	}
	if err := broadcast.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestCatalogFinishThenClose(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	catalog, err := moqmedia.NewCatalogProducer(broadcast)
	if err != nil {
		t.Fatal(err)
	}
	track, err := broadcast.PublishTrack("status", nil)
	if err != nil {
		t.Fatal(err)
	}
	if err := track.Finish(); err != nil {
		t.Fatal(err)
	}
	if err := catalog.Finish(); err != nil {
		t.Fatal(err)
	}
	// Finishing the content leaves the broadcast up until it closes.
	if _, err := broadcast.Consume(); err != nil {
		t.Fatalf("Consume after Finish = %v", err)
	}
	if err := broadcast.Close(); err != nil {
		t.Fatal(err)
	}
	if err := catalog.Finish(); !errors.Is(err, moq.ErrClosed) {
		t.Fatalf("catalog Finish after Close = %v, want closed", err)
	}
}

func TestBroadcastCloseTwiceIsNoop(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	if err := broadcast.Close(); err != nil {
		t.Fatal(err)
	}
	if err := broadcast.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
	if _, err := broadcast.PublishTrack("events", nil); err == nil {
		t.Fatal("PublishTrack after Close succeeded")
	}
}

func TestEncodeAudioWithOpusObject(t *testing.T) {
	// The producer retains the codec, so releasing the codec and the
	// config in either order must still encode.
	for _, codecFirst := range []bool{true, false} {
		broadcast, err := moq.NewBroadcastProducer()
		if err != nil {
			t.Fatal(err)
		}
		codec := moq.OpusAudioCodec()
		name := "mic"
		output := moqaudio.EncoderOutput{Codec: codec, FrameDuration: 20 * time.Millisecond}
		catalog, err := moqmedia.NewCatalogProducer(broadcast)
		if err != nil {
			t.Fatal(err)
		}
		producer, err := moqaudio.NewProducer(catalog, moqaudio.ProducerOptions{
			Input: moqaudio.EncoderInput{
				Format:     moq.AudioSampleFormatF32,
				SampleRate: 48000,
				Channels:   1,
			},
			Output:    output,
			Track:     &name,
			Bandwidth: nil,
		})
		if err != nil {
			t.Fatal(err)
		}
		if codecFirst {
			codec.Destroy()
		}
		// One 20 ms Opus frame of silence at 48 kHz mono.
		if err := producer.Write(moqaudio.Frame{Data: make([]byte, 960*4)}); err != nil {
			t.Fatal(err)
		}
		if !codecFirst {
			codec.Destroy()
		}
		demand, err := producer.Demand()
		if err != nil || demand.Name() != "mic" {
			t.Fatalf("demand name err=%v", err)
		}
		if err := producer.Finish(); err != nil {
			t.Fatal(err)
		}
		if err := broadcast.Close(); err != nil {
			t.Fatal(err)
		}
	}
}

// A fixed playout delay is reported back as is, and an estimated one moves as frames are read.
func TestAudioConsumerDelay(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	origin := newOrigin(t)
	publication, err := origin.CreateBroadcast("audio-delay")
	if err != nil {
		t.Fatal(err)
	}
	broadcast := publication.Broadcast()
	track := "mic"
	catalogProducer, err := moqmedia.NewCatalogProducer(broadcast)
	if err != nil {
		t.Fatal(err)
	}
	audio, err := moqaudio.NewProducer(catalogProducer, moqaudio.ProducerOptions{
		Input:     moqaudio.EncoderInput{Format: moq.AudioSampleFormatF32, SampleRate: 48000, Channels: 1},
		Output:    moqaudio.EncoderOutput{Codec: moq.OpusAudioCodec()},
		Track:     &track,
		Bandwidth: nil,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := publication.Announce(moq.AnnounceRoute{}); err != nil {
		t.Fatal(err)
	}

	bc, err := origin.Consume().Request(ctx, "audio-delay", nil)
	if err != nil {
		t.Fatal(err)
	}
	catalog, err := moqmedia.CatalogSnapshot(ctx, bc)
	if err != nil {
		t.Fatal(err)
	}
	rendition, ok := catalog.Audio[track]
	if !ok {
		t.Fatalf("catalog has no %q rendition: %v", track, catalog.Audio)
	}

	fixedDelay := 80 * time.Millisecond
	fixed, err := moqaudio.Subscribe(ctx, bc, track, rendition, moqaudio.DecoderOutput{Format: moq.AudioSampleFormatF32, Delay: &fixedDelay})
	if err != nil {
		t.Fatal(err)
	}
	defer fixed.Cancel()
	estimated, err := moqaudio.Subscribe(ctx, bc, track, rendition, moqaudio.DecoderOutput{Format: moq.AudioSampleFormatF32})
	if err != nil {
		t.Fatal(err)
	}
	defer estimated.Cancel()
	if got := fixed.Delay(); got != fixedDelay {
		t.Fatalf("fixed delay = %v, want %v", got, fixedDelay)
	}
	initial := estimated.Delay()

	// Two 20 ms Opus frames of silence at 48 kHz mono.
	for i := range 2 {
		if err := audio.Write(moqaudio.Frame{Timestamp: time.Duration(i) * 20 * time.Millisecond, Data: make([]byte, 960*4)}); err != nil {
			t.Fatal(err)
		}
	}
	for _, consumer := range []*moqaudio.Consumer{fixed, estimated} {
		if read, err := consumer.Next(ctx); err != nil {
			t.Fatal(err)
		} else if _, ok := read.(moqaudio.ReadFrame); !ok {
			t.Fatalf("next = %#v, want a frame", read)
		}
	}

	if got := fixed.Delay(); got != fixedDelay {
		t.Fatalf("fixed delay after a read = %v, want %v", got, fixedDelay)
	}
	if got := estimated.Delay(); got == initial {
		t.Fatalf("estimated delay stayed at %v after a read", got)
	}

	if err := audio.Finish(); err != nil {
		t.Fatal(err)
	}
}

// Opus' 2.5 ms frame reaches the encoder, and a duration outside the Opus set is
// refused rather than silently rounded.
func TestEncodeAudioFrameDurations(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	input := moqaudio.EncoderInput{
		Format:     moq.AudioSampleFormatF32,
		SampleRate: 48000,
		Channels:   1,
	}

	catalog, err := moqmedia.NewCatalogProducer(broadcast)
	if err != nil {
		t.Fatal(err)
	}
	fineName := "fine"
	fine, err := moqaudio.NewProducer(catalog, moqaudio.ProducerOptions{
		Input:     input,
		Output:    moqaudio.EncoderOutput{Codec: moq.OpusAudioCodec(), FrameDuration: 2500 * time.Microsecond},
		Track:     &fineName,
		Bandwidth: nil,
	})
	if err != nil {
		t.Fatal(err)
	}
	// 2.5 ms of silence at 48 kHz mono f32: exactly one encoded frame.
	if err := fine.Write(moqaudio.Frame{Data: make([]byte, 120*4)}); err != nil {
		t.Fatal(err)
	}
	if err := fine.Finish(); err != nil {
		t.Fatal(err)
	}

	coarseName := "coarse"
	_, err = moqaudio.NewProducer(catalog, moqaudio.ProducerOptions{
		Input:     input,
		Output:    moqaudio.EncoderOutput{Codec: moq.OpusAudioCodec(), FrameDuration: 2 * time.Millisecond},
		Track:     &coarseName,
		Bandwidth: nil,
	})
	if !errors.Is(err, moq.ErrAudio) {
		t.Fatalf("err = %v, want ErrAudio: 2 ms is not an opus frame duration", err)
	}

	if err := broadcast.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestVideoPropertiesUseDefaultedFields(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	rotation := 315.0
	catalogProducer, err := moqmedia.NewCatalogProducer(broadcast)
	if err != nil {
		t.Fatal(err)
	}
	if err := catalogProducer.SetVideoProperties(moqmedia.VideoProperties{Rotation: &rotation}); err != nil {
		t.Fatal(err)
	}
	if err := broadcast.Close(); err != nil {
		t.Fatal(err)
	}
}

// TestDecodeVideoFrame pins a decoded frame owning its picture: it converts to
// either CPU layout on demand and stays readable after its consumer is
// cancelled, until Close. A surface decode also exposes the platform surface,
// and is refused where no surface variant exists.
func TestDecodeVideoFrame(t *testing.T) {
	t.Run("cpu", func(t *testing.T) { testDecodeVideoFrame(t, false) })
	t.Run("surface", func(t *testing.T) { testDecodeVideoFrame(t, true) })
}

func testDecodeVideoFrame(t *testing.T, surface bool) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	origin := newOrigin(t)
	publication, err := origin.CreateBroadcast("video-decode-frame")
	if err != nil {
		t.Fatal(err)
	}
	broadcast := publication.Broadcast()
	track := "camera"
	catalogProducer, err := moqmedia.NewCatalogProducer(broadcast)
	if err != nil {
		t.Fatal(err)
	}
	video, err := moqvideo.NewProducer(catalogProducer, moqvideo.ProducerOptions{
		Input:     moqvideo.EncoderInput{Format: moq.VideoPixelFormatRgba, Width: 320, Height: 240, Framerate: moqvideo.FrameRate{Numerator: 30000, Denominator: 1001}},
		Output:    moqvideo.EncoderOutput{Codec: moq.VideoCodecH264, Kind: moq.SoftwareEncoder()},
		Track:     &track,
		Bandwidth: nil,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := publication.Announce(moq.AnnounceRoute{}); err != nil {
		t.Fatal(err)
	}

	// Seed the track so a subscriber joining below lands on encoded media.
	rgba := make([]byte, 320*240*4)
	for i := range rgba {
		rgba[i] = 0x80
	}
	if err := video.Cut(); err != nil {
		t.Fatal(err)
	}
	for i := range 10 {
		if err := video.Write(moqvideo.Frame{Timestamp: time.Duration(i) * 33333 * time.Microsecond, Data: rgba}); err != nil {
			t.Fatal(err)
		}
	}

	bc, err := origin.Consume().Request(ctx, "video-decode-frame", nil)
	if err != nil {
		t.Fatal(err)
	}
	catalog, err := moqmedia.CatalogSnapshot(ctx, bc)
	if err != nil {
		t.Fatal(err)
	}
	rendition, ok := catalog.Video[track]
	if !ok {
		t.Fatalf("catalog has no %q rendition: %v", track, catalog.Video)
	}

	decoder, err := moqvideo.Subscribe(ctx, bc, track, rendition, moqvideo.DecoderOutput{Surface: surface})
	if surface && runtime.GOOS != "darwin" {
		if !errors.Is(err, moq.ErrUnsupported) {
			t.Fatalf("surface decode off macOS: err = %v, want ErrUnsupported", err)
		}
		return
	}
	if err != nil {
		t.Fatal(err)
	}

	// Keep the encoder fed so the decoder sees frames after it joined.
	for i := 10; i < 40; i++ {
		if err := video.Write(moqvideo.Frame{Timestamp: time.Duration(i) * 33333 * time.Microsecond, Data: rgba}); err != nil {
			t.Fatal(err)
		}
	}

	read, err := decoder.Next(ctx)
	if err != nil {
		t.Fatal(err)
	}
	decoded, ok := read.(moqvideo.ReadFrame)
	if !ok {
		t.Fatalf("next = %#v, want a frame", read)
	}
	frame := decoded.Frame
	defer frame.Close()

	decoder.Cancel()

	if !surface {
		if got := frame.Surface(); got != nil {
			t.Fatalf("CPU decode surface = %#v, want nil", got)
		}
	} else if pb, ok := frame.Surface().(moq.VideoSurfacePixelBuffer); !ok || pb.Pointer == 0 {
		t.Fatalf("surface = %#v, want a non-null VideoSurfacePixelBuffer", frame.Surface())
	}

	i420, err := frame.Pixels(moq.VideoPixelFormatI420)
	if err != nil {
		t.Fatal(err)
	}
	if want := int(frame.Width()) * int(frame.Height()) * 3 / 2; len(i420) != want {
		t.Fatalf("I420 length = %d, want %d", len(i420), want)
	}

	packed, err := frame.Pixels(moq.VideoPixelFormatRgba)
	if err != nil {
		t.Fatal(err)
	}
	if want := int(frame.Width()) * int(frame.Height()) * 4; len(packed) != want {
		t.Fatalf("RGBA length = %d, want %d", len(packed), want)
	}
	for i := 3; i < len(packed); i += 4 {
		if packed[i] != 0xFF {
			t.Fatalf("RGBA alpha at %d = %#x, want 0xff", i, packed[i])
		}
	}

	if err := video.Finish(); err != nil {
		t.Fatal(err)
	}
	if err := broadcast.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestInsertDatagramAndLargest(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	track, err := broadcast.PublishTrack("events", timedInfo())
	if err != nil {
		t.Fatal(err)
	}
	consumer, err := track.Consume(nil)
	if err != nil {
		t.Fatal(err)
	}
	if largest, err := track.Largest(); err != nil || largest != nil {
		t.Fatalf("empty track: largest %v, err %v", largest, err)
	}

	if err := track.InsertDatagram(7, moq.Frame{Payload: []byte("seven"), Timestamp: ts(0)}); err != nil {
		t.Fatal(err)
	}
	datagram, err := consumer.RecvDatagram(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if datagram == nil || datagram.Sequence != 7 || string(datagram.Payload) != "seven" {
		t.Fatalf("got datagram %+v", datagram)
	}
	largest, err := track.Largest()
	if err != nil {
		t.Fatal(err)
	}
	if largest == nil || *largest != (moq.Position{Group: 7, Frame: 0}) {
		t.Fatalf("largest %+v", largest)
	}
	if next, err := track.AppendDatagram(moq.Frame{Payload: []byte("next"), Timestamp: ts(0)}); err != nil || next != 8 {
		t.Fatalf("appended at %d, err %v", next, err)
	}
}

func TestBroadcastConsumerClosed(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	consumer, err := broadcast.Consume()
	if err != nil {
		t.Fatal(err)
	}
	closed := make(chan error, 1)
	go func() { closed <- consumer.Closed(ctx) }()
	if err := broadcast.Close(); err != nil {
		t.Fatal(err)
	}
	if err := <-closed; err != nil {
		t.Fatal(err)
	}
}

func TestFetchGroupAndServeDynamicMiss(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	track, err := broadcast.PublishTrack("events", timedInfo())
	if err != nil {
		t.Fatal(err)
	}
	consumer, err := broadcast.Consume()
	if err != nil {
		t.Fatal(err)
	}

	cached, err := track.AppendGroup()
	if err != nil {
		t.Fatal(err)
	}
	if err := cached.WriteFrame(moq.Frame{Payload: []byte("cached"), Timestamp: ts(0)}); err != nil {
		t.Fatal(err)
	}
	if err := cached.Finish(); err != nil {
		t.Fatal(err)
	}

	fetched, err := consumer.FetchGroup(ctx, "events", 0, &moq.FetchGroupOptions{Priority: 3})
	if err != nil {
		t.Fatal(err)
	}
	frame, err := fetched.ReadFrame(ctx)
	if err != nil || frame == nil || string(frame.Payload) != "cached" {
		t.Fatalf("cached fetch: frame=%+v err=%v", frame, err)
	}

	negative := -time.Millisecond
	if _, err := consumer.FetchGroup(ctx, "events", 0, &moq.FetchGroupOptions{MaxDelay: &negative}); err == nil {
		t.Fatal("a negative fetch max delay was accepted, want an error")
	}

	dynamic, err := track.Requests()
	if err != nil {
		t.Fatal(err)
	}
	type fetchResult struct {
		group *moq.GroupConsumer
		err   error
	}
	result := make(chan fetchResult, 1)
	go func() {
		group, err := consumer.FetchGroup(ctx, "events", 7, &moq.FetchGroupOptions{Priority: 11})
		result <- fetchResult{group: group, err: err}
	}()

	fill, err := dynamic.RequestedFill(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if fill.StartGroup() != 7 || fill.EndGroup() != 8 || fill.Priority() != 11 {
		t.Fatalf("unexpected fill: start=%d end=%d priority=%d", fill.StartGroup(), fill.EndGroup(), fill.Priority())
	}
	if next := fill.Next(); next == nil || *next != 7 || !fill.IsUsed() {
		t.Fatalf("unexpected fill: next=%v used=%v", next, fill.IsUsed())
	}
	if !fill.IsWanted(7) || fill.IsWanted(8) {
		t.Fatalf("unexpected wants: 7=%v 8=%v", fill.IsWanted(7), fill.IsWanted(8))
	}
	produced, err := fill.Accept(7)
	if err != nil {
		t.Fatal(err)
	}
	if err := produced.WriteFrame(moq.Frame{Payload: []byte("archive"), Timestamp: ts(7 * 20_000 * time.Microsecond)}); err != nil {
		t.Fatal(err)
	}
	if err := produced.Finish(); err != nil {
		t.Fatal(err)
	}

	res := <-result
	if res.err != nil {
		t.Fatal(res.err)
	}
	frame, err = res.group.ReadFrame(ctx)
	if err != nil || frame == nil || string(frame.Payload) != "archive" {
		t.Fatalf("dynamic fetch: frame=%+v err=%v", frame, err)
	}
}

func TestUnknownFormat(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	// A bad format is no longer expressible: it is an enum. Bad init bytes still are.
	if _, err := moqmedia.NewAudioTrackProducer(broadcast, moqmedia.Named{}, moqmedia.AudioInit{Format: moqmedia.AudioFormatOpus, Data: nil}); err == nil {
		t.Fatal("expected error for unknown format")
	}
}

func TestLocalPublishConsumeAudio(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	origin := newOrigin(t)
	publication, err := origin.CreateBroadcast("live")
	if err != nil {
		t.Fatal(err)
	}
	broadcast := publication.Broadcast()
	media, err := moqmedia.NewAudioTrackProducer(broadcast, moqmedia.Named{}, moqmedia.AudioInit{Format: moqmedia.AudioFormatOpus, Data: opusHead()})
	if err != nil {
		t.Fatal(err)
	}
	if err := publication.Announce(moq.AnnounceRoute{}); err != nil {
		t.Fatal(err)
	}

	consumer := origin.Consume()
	announced, err := consumer.Announced(moq.AnnounceOptions{})
	if err != nil {
		t.Fatal(err)
	}
	defer announced.Cancel()

	ann := nextAnnounced(t, ctx, announced)
	if ann.Prefix != "live" {
		t.Fatalf("prefix = %q, want %q", ann.Prefix, "live")
	}
	if len(ann.Route.Hops) != 0 {
		t.Fatalf("route hops = %v, want empty for local origin", ann.Route.Hops)
	}

	bc, err := consumer.Request(ctx, ann.Prefix, nil)
	if err != nil {
		t.Fatal(err)
	}

	catalog, err := moqmedia.CatalogSnapshot(ctx, bc)
	if err != nil {
		t.Fatal(err)
	}
	if len(catalog.Audio) != 1 || len(catalog.Video) != 0 {
		t.Fatalf("catalog audio=%d video=%d, want 1/0", len(catalog.Audio), len(catalog.Video))
	}

	var trackName string
	var audio moqmedia.Audio
	for name, a := range catalog.Audio {
		trackName, audio = name, a
	}
	if audio.Codec != "opus" || audio.SampleRate != 48000 || audio.ChannelCount != 2 {
		t.Fatalf("audio = %+v, want opus/48000/2", audio)
	}

	mediaConsumer, err := moqmedia.NewTrackConsumer(ctx, bc, moqmedia.TrackConfig{Rendition: moqmedia.AudioRendition(trackName, audio)})
	if err != nil {
		t.Fatal(err)
	}
	defer mediaConsumer.Cancel()

	payload := []byte("opus audio payload data")
	if err := media.WriteFrame(moq.Frame{Payload: payload, Timestamp: ts(1_000_000 * time.Microsecond)}); err != nil {
		t.Fatal(err)
	}

	read, err := mediaConsumer.Next(ctx)
	if err != nil {
		t.Fatal(err)
	}
	value, ok := read.(moqmedia.ReadFrame)
	if !ok {
		t.Fatalf("expected a media frame, got %T", read)
	}
	frame := &value.Frame
	if string(frame.Payload) != string(payload) || uint64(frame.Timestamp.Microseconds()) != 1_000_000 {
		t.Fatalf("frame = %+v, want payload=%q ts=1000000", frame, payload)
	}

	// The same group, received raw in arrival order and decoded with the container.
	track, err := bc.SubscribeTrack(ctx, trackName, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer track.Cancel()
	group, err := track.RecvGroup(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if group == nil {
		t.Fatal("expected a group")
	}
	decoded, err := moqmedia.NewGroupConsumer(group, audio.Container)
	if err != nil {
		t.Fatal(err)
	}
	defer decoded.Cancel()
	frame, err = decoded.Next(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if frame == nil || string(frame.Payload) != string(payload) || uint64(frame.Timestamp.Microseconds()) != 1_000_000 {
		t.Fatalf("decoded frame = %+v, want payload=%q ts=1000000", frame, payload)
	}
}

func TestTrackPublishConsume(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	track, err := broadcast.PublishTrack("data", timedInfo())
	if err != nil {
		t.Fatal(err)
	}
	consumer, err := track.Consume(nil)
	if err != nil {
		t.Fatal(err)
	}
	defer consumer.Cancel()

	if err := track.WriteFrame(moq.Frame{Payload: []byte("hello"), Timestamp: ts(12_345 * time.Microsecond)}); err != nil {
		t.Fatal(err)
	}

	frame, err := consumer.ReadFrame(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if frame == nil {
		t.Fatal("expected a frame")
	}
	if string(frame.Payload) != "hello" || frame.Timestamp == nil || *frame.Timestamp != 12_345*time.Microsecond {
		t.Fatalf("frame = %+v, want payload=hello ts=12345", frame)
	}

	group, err := track.AppendGroup()
	if err != nil {
		t.Fatal(err)
	}
	groupConsumer, err := group.Consume()
	if err != nil {
		t.Fatal(err)
	}
	defer groupConsumer.Cancel()
	if err := group.WriteFrame(moq.Frame{Payload: []byte("group"), Timestamp: ts(23_456 * time.Microsecond)}); err != nil {
		t.Fatal(err)
	}
	if err := group.Finish(); err != nil {
		t.Fatal(err)
	}
	frame, err = groupConsumer.ReadFrame(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if frame == nil {
		t.Fatal("expected a group frame")
	}
	if string(frame.Payload) != "group" || frame.Timestamp == nil || *frame.Timestamp != 23_456*time.Microsecond {
		t.Fatalf("frame = %+v, want payload=group ts=23456", frame)
	}
}

func TestAscendingSubscriptionReadsCachedBacklogOldestFirst(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	track, err := broadcast.PublishTrack("backlog", timedInfo())
	if err != nil {
		t.Fatal(err)
	}
	for i := range 3 {
		group, err := track.AppendGroup()
		if err != nil {
			t.Fatal(err)
		}
		if err := group.WriteFrame(moq.Frame{Payload: []byte("x"), Timestamp: ts(time.Duration(i) * time.Millisecond)}); err != nil {
			t.Fatal(err)
		}
		if err := group.Finish(); err != nil {
			t.Fatal(err)
		}
	}

	order := moq.OrderAscending
	maxDelay := 10 * time.Second
	consumer, err := track.Consume(&moq.Subscription{
		MaxDelay: &maxDelay,
		Ranges:   []moq.Range{{Start: moq.AnchorRelative{Groups: 2}}},
		Order:    &order,
	})
	if err != nil {
		t.Fatal(err)
	}
	defer consumer.Cancel()

	var sequences []uint64
	for range 3 {
		group, err := consumer.RecvGroup(ctx)
		if err != nil {
			t.Fatal(err)
		}
		if group == nil {
			t.Fatal("expected a group")
		}
		sequences = append(sequences, group.Sequence())
	}
	if !slices.Equal(sequences, []uint64{0, 1, 2}) {
		t.Fatalf("sequences = %v, want [0 1 2]", sequences)
	}
}

func TestReadFrameSkipsEmptyThenPopulatedGroups(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	track, err := broadcast.PublishTrack("status", timedInfo())
	if err != nil {
		t.Fatal(err)
	}
	consumer, err := track.Consume(nil)
	if err != nil {
		t.Fatal(err)
	}
	defer consumer.Cancel()

	empty, err := track.AppendGroup()
	if err != nil {
		t.Fatal(err)
	}
	if err := empty.Finish(); err != nil {
		t.Fatal(err)
	}
	if err := track.WriteFrame(moq.Frame{Payload: []byte("populated"), Timestamp: ts(2_000 * time.Microsecond)}); err != nil {
		t.Fatal(err)
	}

	frame, err := consumer.ReadFrame(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if frame == nil || string(frame.Payload) != "populated" || frame.Timestamp == nil || *frame.Timestamp != 2_000*time.Microsecond {
		t.Fatalf("frame = %+v, want payload=populated ts=2000", frame)
	}
}

func TestTrackSparseGroupsAndKnownEnd(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	track, err := broadcast.PublishTrack("sparse", nil)
	if err != nil {
		t.Fatal(err)
	}
	group, err := track.CreateGroup(2)
	if err != nil {
		t.Fatal(err)
	}
	if group.Sequence() != 2 {
		t.Fatalf("sequence = %d, want 2", group.Sequence())
	}
	if err := group.Finish(); err != nil {
		t.Fatal(err)
	}
	if err := track.FinishAt(5); err != nil {
		t.Fatal(err)
	}
	group, err = track.CreateGroup(4)
	if err != nil {
		t.Fatal(err)
	}
	if err := group.Finish(); err != nil {
		t.Fatal(err)
	}
	if _, err := track.CreateGroup(5); err == nil {
		t.Fatal("expected group at final sequence to fail")
	}
	if err := track.Finish(); err != nil {
		t.Fatal(err)
	}
}

func TestDynamicTrackRequest(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()

	dynamic, err := broadcast.Requests()
	if err != nil {
		t.Fatal(err)
	}
	defer dynamic.Cancel()

	consumer, err := broadcast.Consume()
	if err != nil {
		t.Fatal(err)
	}

	type subscribeResult struct {
		track *moq.TrackConsumer
		err   error
	}
	subscribe := make(chan subscribeResult, 1)
	go func() {
		track, err := consumer.SubscribeTrack(ctx, "events", nil)
		subscribe <- subscribeResult{track: track, err: err}
	}()

	request, err := dynamic.RequestedTrack(ctx)
	if err != nil {
		t.Fatal(err)
	}
	name, err := request.Name()
	if err != nil {
		t.Fatal(err)
	}
	if name != "events" {
		t.Fatalf("request name = %q, want events", name)
	}

	track, err := request.Accept(timedInfo())
	if err != nil {
		t.Fatal(err)
	}
	payload := []byte("hello dynamic track")
	if err := track.WriteFrame(moq.Frame{Payload: payload, Timestamp: ts(0)}); err != nil {
		t.Fatal(err)
	}

	var trackConsumer *moq.TrackConsumer
	select {
	case res := <-subscribe:
		if res.err != nil {
			t.Fatal(res.err)
		}
		trackConsumer = res.track
	case <-ctx.Done():
		t.Fatal(ctx.Err())
	}
	defer trackConsumer.Cancel()

	frame, err := trackConsumer.ReadFrame(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if frame == nil || string(frame.Payload) != string(payload) || frame.Timestamp == nil || *frame.Timestamp != 0 {
		t.Fatalf("frame = %+v, want payload=%q ts=0", frame, payload)
	}
	if err := track.Finish(); err != nil {
		t.Fatal(err)
	}
}

func TestDynamicTrackRequestCanPublishAudio(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()

	dynamic, err := broadcast.Requests()
	if err != nil {
		t.Fatal(err)
	}
	defer dynamic.Cancel()

	consumer, err := broadcast.Consume()
	if err != nil {
		t.Fatal(err)
	}

	type subscribeResult struct {
		media *moqmedia.TrackConsumer
		err   error
	}
	subscribe := make(chan subscribeResult, 1)
	go func() {
		media, err := moqmedia.NewTrackConsumer(ctx, consumer, moqmedia.TrackConfig{Rendition: moqmedia.AudioRendition("requested-audio", moqmedia.Audio{Codec: "opus", SampleRate: 48000, ChannelCount: 2, Enabled: true, Container: moqmedia.LegacyContainer()})})
		subscribe <- subscribeResult{media: media, err: err}
	}()

	request, err := dynamic.RequestedTrack(ctx)
	if err != nil {
		t.Fatal(err)
	}
	name, err := request.Name()
	if err != nil {
		t.Fatal(err)
	}
	if name != "requested-audio" {
		t.Fatalf("request name = %q, want requested-audio", name)
	}

	media, err := moqmedia.NewAudioTrackProducer(broadcast, moqmedia.Requested{Request: request}, moqmedia.AudioInit{Format: moqmedia.AudioFormatOpus, Data: opusHead()})
	if err != nil {
		t.Fatal(err)
	}
	mediaDemand, err := media.Demand()
	if err != nil {
		t.Fatal(err)
	}
	mediaName := mediaDemand.Name()
	if mediaName != "requested-audio" {
		t.Fatalf("media name = %q, want requested-audio", mediaName)
	}
	if _, err := request.Name(); !errors.Is(err, moq.ErrClosed) {
		t.Fatalf("request name after accept error = %v, want ErrClosed", err)
	}

	var mediaConsumer *moqmedia.TrackConsumer
	select {
	case res := <-subscribe:
		if res.err != nil {
			t.Fatal(res.err)
		}
		mediaConsumer = res.media
	case <-ctx.Done():
		t.Fatal(ctx.Err())
	}
	defer mediaConsumer.Cancel()

	payload := []byte("dynamic opus frame")
	if err := media.WriteFrame(moq.Frame{Payload: payload, Timestamp: ts(20_000 * time.Microsecond)}); err != nil {
		t.Fatal(err)
	}

	read, err := mediaConsumer.Next(ctx)
	if err != nil {
		t.Fatal(err)
	}
	value, ok := read.(moqmedia.ReadFrame)
	if !ok {
		t.Fatalf("expected a media frame, got %T", read)
	}
	frame := &value.Frame
	if string(frame.Payload) != string(payload) || uint64(frame.Timestamp.Microseconds()) != 20_000 {
		t.Fatalf("frame = %+v, want payload=%q ts=20000", frame, payload)
	}
	// The explicit pause marker is an encoder end even while the track stays open.
	if err := media.Discontinuity(); err != nil {
		t.Fatal(err)
	}
	if err := media.Finish(); err != nil {
		t.Fatal(err)
	}
	end, err := mediaConsumer.Next(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if value, ok := end.(moqmedia.ReadEnd); !ok || value.End != 20_000*time.Microsecond {
		t.Fatalf("expected encoder end, got %T", end)
	}
	if next, err := mediaConsumer.Next(ctx); next != nil || err != nil {
		t.Fatalf("after finish: %v %v", next, err)
	}
}

// TestRecvGroupCancelRace exercises the core bridge.Call path under -race:
// the native RecvGroup runs on an internal goroutine while ctx expiry triggers a
// concurrent Cancel on the same consumer. No group is ever written, so each read
// blocks until its short ctx fires. The race detector flags any unsynchronized
// access between the in-flight call and the cancel.
func TestRecvGroupCancelRace(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()

	var wg sync.WaitGroup
	for i := 0; i < 16; i++ {
		track, err := broadcast.PublishTrack(fmt.Sprintf("t%d", i), nil)
		if err != nil {
			t.Fatal(err)
		}
		consumer, err := track.Consume(nil)
		if err != nil {
			t.Fatal(err)
		}

		wg.Add(1)
		go func(c *moq.TrackConsumer) {
			defer wg.Done()
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Millisecond)
			defer cancel()
			// Returns ctx.Err() once the deadline fires; we only care that it
			// returns without a data race or panic.
			_, _ = c.RecvGroup(ctx)
		}(consumer)
	}
	wg.Wait()
}

// TestConsumerCancelConcurrent confirms Cancel is safe to call repeatedly from
// multiple goroutines (it underlies every stream's cleanup and Close path).
func TestConsumerCancelConcurrent(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()

	track, err := broadcast.PublishTrack("x", nil)
	if err != nil {
		t.Fatal(err)
	}
	consumer, err := track.Consume(nil)
	if err != nil {
		t.Fatal(err)
	}

	var wg sync.WaitGroup
	for i := 0; i < 8; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			consumer.Cancel()
		}()
	}
	wg.Wait()
}

// TestRequestCancelKeepsTheOrigin cancels a Request parked on a
// dynamic handler, then proves the origin still resolves: the cancel has to abort
// that one request rather than the consumer it was made on.
func TestRequestCancelKeepsTheOrigin(t *testing.T) {
	origin := newOrigin(t)
	dynamic, err := origin.Requests("")
	if err != nil {
		t.Fatal(err)
	}
	defer dynamic.Cancel()
	consumer := origin.Consume()

	waitCtx, waitCancel := context.WithTimeout(context.Background(), testTimeout)
	defer waitCancel()

	ctx, cancel := context.WithCancel(context.Background())
	requested := make(chan error, 1)
	go func() {
		_, err := consumer.Request(ctx, "never/served", nil)
		requested <- err
	}()

	// Take the request but never answer it, so the cancel lands on a call that is
	// genuinely parked rather than one that has not started.
	pending, err := dynamic.RequestedBroadcast(waitCtx)
	if err != nil {
		t.Fatal(err)
	}
	cancel()

	select {
	case err := <-requested:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("err = %v, want context.Canceled", err)
		}
	case <-waitCtx.Done():
		t.Fatal("Request did not return after its context was cancelled")
	}
	_ = pending.Reject(0)

	// The same consumer resolves the next path, which it could not do if the
	// cancel had torn the origin down.
	resolved := make(chan error, 1)
	go func() {
		_, err := consumer.Request(waitCtx, "later/served", nil)
		resolved <- err
	}()

	next, err := dynamic.RequestedBroadcast(waitCtx)
	if err != nil {
		t.Fatal(err)
	}
	prepared, err := next.Prepare(moq.MintEpoch())
	if err != nil {
		t.Fatal(err)
	}
	served, err := prepared.Publish()
	if err != nil {
		t.Fatal(err)
	}
	defer served.Broadcast().Close()
	select {
	case err := <-resolved:
		if err != nil {
			t.Fatal(err)
		}
	case <-waitCtx.Done():
		t.Fatal(waitCtx.Err())
	}
}

// TestSubscribeTrackCancelKeepsTheBroadcast cancels a SubscribeTrack parked on a
// dynamic producer that has not accepted the track, then subscribes again on the
// same broadcast consumer.
func TestSubscribeTrackCancelKeepsTheBroadcast(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()

	dynamic, err := broadcast.Requests()
	if err != nil {
		t.Fatal(err)
	}
	defer dynamic.Cancel()

	consumer, err := broadcast.Consume()
	if err != nil {
		t.Fatal(err)
	}

	waitCtx, waitCancel := context.WithTimeout(context.Background(), testTimeout)
	defer waitCancel()

	ctx, cancel := context.WithCancel(context.Background())
	subscribed := make(chan error, 1)
	go func() {
		_, err := consumer.SubscribeTrack(ctx, "never", nil)
		subscribed <- err
	}()

	pending, err := dynamic.RequestedTrack(waitCtx)
	if err != nil {
		t.Fatal(err)
	}
	cancel()

	select {
	case err := <-subscribed:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("err = %v, want context.Canceled", err)
		}
	case <-waitCtx.Done():
		t.Fatal("SubscribeTrack did not return after its context was cancelled")
	}
	_ = pending.Abort(0)

	resolved := make(chan error, 1)
	go func() {
		_, err := consumer.SubscribeTrack(waitCtx, "later", nil)
		resolved <- err
	}()

	next, err := dynamic.RequestedTrack(waitCtx)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := next.Accept(nil); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-resolved:
		if err != nil {
			t.Fatal(err)
		}
	case <-waitCtx.Done():
		t.Fatal(waitCtx.Err())
	}
}

// TestUsedCancelKeepsTheTrack cancels a demand Used wait, which has no
// object-wide cancel to fall back on, and confirms the track still publishes.
func TestUsedCancelKeepsTheTrack(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()

	track, err := broadcast.PublishTrack("status", timedInfo())
	if err != nil {
		t.Fatal(err)
	}
	demand, err := track.Demand()
	if err != nil {
		t.Fatal(err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	if err := untilWanted(ctx, demand, true); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("untilWanted error = %v, want context.DeadlineExceeded", err)
	}

	readCtx, readCancel := context.WithTimeout(context.Background(), testTimeout)
	defer readCancel()

	consumer, err := track.Consume(nil)
	if err != nil {
		t.Fatal(err)
	}
	defer consumer.Cancel()
	if err := untilWanted(readCtx, demand, true); err != nil {
		t.Fatal(err)
	}

	payload := []byte("still publishing")
	if err := track.WriteFrame(moq.Frame{Payload: payload, Timestamp: ts(0)}); err != nil {
		t.Fatal(err)
	}
	frame, err := consumer.ReadFrame(readCtx)
	if err != nil {
		t.Fatal(err)
	}
	if frame == nil || string(frame.Payload) != string(payload) {
		t.Fatalf("frame = %+v, want payload=%q", frame, payload)
	}
}

// TestCancelDoesNotLeakGoroutines parks many subscribes on a dynamic producer
// that never answers, cancels them all, and waits for the goroutine count to come
// back. Each parked call holds a goroutine inside cgo, so a cancel that returned
// ctx.Err() without aborting the native task would strand all of them.
func TestCancelDoesNotLeakGoroutines(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()

	dynamic, err := broadcast.Requests()
	if err != nil {
		t.Fatal(err)
	}
	defer dynamic.Cancel()

	consumer, err := broadcast.Consume()
	if err != nil {
		t.Fatal(err)
	}

	waitCtx, waitCancel := context.WithTimeout(context.Background(), testTimeout)
	defer waitCancel()

	const parked = 32
	baseline := runtime.NumGoroutine()

	ctx, cancel := context.WithCancel(context.Background())
	var wg sync.WaitGroup
	for i := range parked {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			_, err := consumer.SubscribeTrack(ctx, fmt.Sprintf("never-%d", i), nil)
			if !errors.Is(err, context.Canceled) {
				t.Errorf("SubscribeTrack error = %v, want context.Canceled", err)
			}
		}(i)
	}

	// Drain the requests so every subscribe is parked on one before we cancel.
	pending := make([]*moq.TrackRequest, 0, parked)
	for range parked {
		request, err := dynamic.RequestedTrack(waitCtx)
		if err != nil {
			t.Fatal(err)
		}
		pending = append(pending, request)
	}

	cancel()
	wg.Wait()

	// The requests stay pending on purpose: nothing but the cancel can unwind the
	// native subscribes, so a count that comes back proves the cancel reached them.
	// Allow the scheduler some slack rather than the 32 a leak would leave behind.
	deadline := time.Now().Add(testTimeout)
	for runtime.NumGoroutine() > baseline+parked/4 {
		if time.Now().After(deadline) {
			t.Fatalf("goroutines = %d, want back near the baseline of %d", runtime.NumGoroutine(), baseline)
		}
		time.Sleep(10 * time.Millisecond)
	}

	runtime.KeepAlive(pending)
	for _, request := range pending {
		_ = request.Abort(0)
	}
}

func TestBroadcastIsReachableOnlyWhileAnnounced(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	origin := newOrigin(t)
	publication, err := origin.CreateBroadcast("live")
	if err != nil {
		t.Fatal(err)
	}
	broadcast := publication.Broadcast()
	if _, err := broadcast.PublishTrack("events", nil); err != nil {
		t.Fatal(err)
	}
	consumer := origin.Consume()
	if _, err := consumer.Request(ctx, "live", nil); err == nil {
		t.Fatal("an unannounced broadcast must be unroutable")
	}

	if err := publication.Announce(moq.AnnounceRoute{Cost: 3}); err != nil {
		t.Fatal(err)
	}
	announced, err := consumer.Announced(moq.AnnounceOptions{})
	if err != nil {
		t.Fatal(err)
	}
	defer announced.Cancel()

	if ann := nextAnnounced(t, ctx, announced); ann.Prefix != "live" || ann.Route.Cost != 3 {
		t.Fatalf("announce: ann=%+v", ann)
	}

	if err := publication.Unannounce(); err != nil {
		t.Fatal(err)
	}
	event := nextRoute(t, ctx, announced)
	if retracted, ok := event.(moq.AnnounceEventEnd); !ok || retracted.Announce.Prefix != "live" {
		t.Fatalf("unannounce: event=%+v", event)
	}
	if _, err := consumer.Request(ctx, "live", nil); err == nil {
		t.Fatal("an unannounced broadcast must be unroutable")
	}

	if err := publication.Announce(moq.AnnounceRoute{}); err != nil {
		t.Fatal(err)
	}
	nextAnnounced(t, ctx, announced)
	if _, err := consumer.Request(ctx, "live", nil); err != nil {
		t.Fatal(err)
	}
	if err := broadcast.Close(); err != nil {
		t.Fatal(err)
	}
}

// A publication outliving its closed broadcast cannot announce or withdraw the
// broadcast that replaced it at the same path.
func TestStalePublicationLeavesItsReplacementAlone(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	origin := newOrigin(t)
	stale, err := origin.CreateBroadcast("live")
	if err != nil {
		t.Fatal(err)
	}
	if err := stale.Announce(moq.AnnounceRoute{}); err != nil {
		t.Fatal(err)
	}
	if err := stale.Broadcast().Close(); err != nil {
		t.Fatal(err)
	}

	replacement, err := origin.CreateBroadcast("live")
	if err != nil {
		t.Fatal(err)
	}
	defer replacement.Broadcast().Close()
	if err := replacement.Announce(moq.AnnounceRoute{}); err != nil {
		t.Fatal(err)
	}
	if err := stale.Announce(moq.AnnounceRoute{}); !errors.Is(err, moq.ErrClosed) {
		t.Fatalf("stale Announce = %v, want ErrClosed", err)
	}
	if err := stale.Unannounce(); !errors.Is(err, moq.ErrClosed) {
		t.Fatalf("stale Unannounce = %v, want ErrClosed", err)
	}
	if _, err := origin.Consume().Request(ctx, "live", nil); err != nil {
		t.Fatalf("Request = %v, want the replacement", err)
	}
}

func TestAnnouncedPatternCaptures(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	origin := newOrigin(t)
	filter := "*/chat"
	announced, err := origin.Consume().Announced(moq.AnnounceOptions{Prefix: "room", Filter: &filter})
	if err != nil {
		t.Fatal(err)
	}
	defer announced.Cancel()

	audio, err := origin.CreateBroadcast("room/alice/audio")
	if err != nil {
		t.Fatal(err)
	}
	if err := audio.Announce(moq.AnnounceRoute{}); err != nil {
		t.Fatal(err)
	}
	chat, err := origin.CreateBroadcast("room/alice/chat")
	if err != nil {
		t.Fatal(err)
	}
	if err := chat.Announce(moq.AnnounceRoute{}); err != nil {
		t.Fatal(err)
	}

	update := nextAnnounced(t, ctx, announced)
	if update.Prefix != "room/alice/chat" {
		t.Fatalf("update = %+v, want room/alice/chat", update)
	}
	if update.Captures == nil || len(*update.Captures) != 1 || (*update.Captures)[0] != "alice" {
		t.Fatalf("captures = %v, want [alice]", update.Captures)
	}
}

// An exact filter with no wildcards still reports a full match: captures is
// empty but not nil.
func TestAnnouncedExactFilterCapturesEmpty(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	origin := newOrigin(t)
	filter := ""
	announced, err := origin.Consume().Announced(moq.AnnounceOptions{Prefix: "room/alice/chat", Filter: &filter})
	if err != nil {
		t.Fatal(err)
	}
	defer announced.Cancel()

	chat, err := origin.CreateBroadcast("room/alice/chat")
	if err != nil {
		t.Fatal(err)
	}
	if err := chat.Announce(moq.AnnounceRoute{}); err != nil {
		t.Fatal(err)
	}

	update := nextAnnounced(t, ctx, announced)
	if update.Prefix != "room/alice/chat" {
		t.Fatalf("update = %+v, want room/alice/chat", update)
	}
	if update.Captures == nil || len(*update.Captures) != 0 {
		t.Fatalf("captures = %#v, want a non-nil empty slice", update.Captures)
	}
}

// A route carries the publisher's epoch, and a new one restarts announce consumers.
func TestAnnouncedEpochRestarts(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	origin := newOrigin(t)
	announced, err := origin.Consume().Announced(moq.AnnounceOptions{})
	if err != nil {
		t.Fatal(err)
	}
	defer announced.Cancel()

	publication, err := origin.CreateBroadcast("epoch")
	if err != nil {
		t.Fatal(err)
	}
	first := moq.MintEpoch()
	if err := publication.Announce(moq.AnnounceRoute{Epoch: &first}); err != nil {
		t.Fatal(err)
	}
	if start := nextAnnounced(t, ctx, announced); start.Route.Epoch == nil || *start.Route.Epoch != first {
		t.Fatalf("start epoch = %v, want %s", start.Route.Epoch, first)
	}

	second := moq.MintEpoch()
	if err := publication.Announce(moq.AnnounceRoute{Epoch: &second}); err != nil {
		t.Fatal(err)
	}
	restart, ok := nextRoute(t, ctx, announced).(moq.AnnounceEventRestart)
	if !ok || restart.Announce.Route.Epoch == nil || *restart.Announce.Route.Epoch != second {
		t.Fatalf("restart = %+v, want epoch %s", restart, second)
	}

	bad := "not-an-epoch"
	if err := publication.Announce(moq.AnnounceRoute{Epoch: &bad}); !errors.Is(err, moq.ErrInvalidEpoch) {
		t.Fatalf("Announce(bad epoch) = %v, want ErrInvalidEpoch", err)
	}
}

// nextRoute returns the next announce event.
func nextRoute(t *testing.T, ctx context.Context, announced *moq.AnnounceConsumer) moq.AnnounceEvent {
	t.Helper()

	event, err := announced.Next(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if event == nil {
		t.Fatal("announcement stream ended")
	}
	return event
}

// nextAnnounced returns the next newly announced route.
func nextAnnounced(t *testing.T, ctx context.Context, announced *moq.AnnounceConsumer) moq.Announce {
	t.Helper()

	event := nextRoute(t, ctx, announced)
	announcedEvent, ok := event.(moq.AnnounceEventStart)
	if !ok {
		t.Fatalf("expected an announcement, got %+v", event)
	}
	return announcedEvent.Announce
}

func TestDynamicServesARequestUnderAPrefix(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	origin := newOrigin(t)
	dynamic, err := origin.Requests("live")
	if err != nil {
		t.Fatal(err)
	}
	defer dynamic.Cancel()

	consumer := origin.Consume()
	done := make(chan error, 1)
	go func() {
		_, err := consumer.Request(ctx, "live/cam", nil)
		done <- err
	}()

	request, err := dynamic.RequestedBroadcast(ctx)
	if err != nil {
		t.Fatal(err)
	}
	path, err := request.Path()
	if err != nil || path != "live/cam" {
		t.Fatalf("path = %q err=%v", path, err)
	}
	prepared, err := request.Prepare(moq.MintEpoch())
	if err != nil {
		t.Fatal(err)
	}
	served, err := prepared.Publish()
	if err != nil {
		t.Fatal(err)
	}
	defer served.Broadcast().Close()
	if err := <-done; err != nil {
		t.Fatal(err)
	}
}

func TestClosingAPreparedAnswerRefusesTheRequest(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	origin := newOrigin(t)
	dynamic, err := origin.Requests("live")
	if err != nil {
		t.Fatal(err)
	}
	defer dynamic.Cancel()

	consumer := origin.Consume()
	done := make(chan error, 1)
	go func() {
		_, err := consumer.Request(ctx, "live/cam", nil)
		done <- err
	}()

	request, err := dynamic.RequestedBroadcast(ctx)
	if err != nil {
		t.Fatal(err)
	}
	prepared, err := request.Prepare(moq.MintEpoch())
	if err != nil {
		t.Fatal(err)
	}
	prepared.Close()

	select {
	case err := <-done:
		if err == nil {
			t.Fatal("Request succeeded, want the abandoned answer refused")
		}
		if ctx.Err() != nil {
			t.Fatalf("Request err = %v, want a refusal before the deadline", err)
		}
	case <-ctx.Done():
		t.Fatal(ctx.Err())
	}
}

// us is a raw frame's optional timestamp in microseconds.
func us(v uint64) *uint64 { return &v }

func ts(d time.Duration) *time.Duration { return &d }

// timedInfo declares the microsecond timeline used by timed raw fixtures.
func timedInfo() *moq.TrackInfo {
	scale := uint64(1_000_000)
	return &moq.TrackInfo{Timescale: &scale}
}

// untilWanted blocks until some subscriber wants the track, or none does.
func untilWanted(ctx context.Context, demand *moq.TrackDemand, wanted bool) error {
	for {
		next, err := demand.WantedChanged(ctx)
		if err != nil {
			return err
		}
		if (next != nil) == wanted {
			return nil
		}
	}
}
