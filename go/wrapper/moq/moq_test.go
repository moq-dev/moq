package moq_test

import (
	"context"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/moq-dev/moq-go/moq"
)

// testTimeout bounds the blocking stream calls so a regression fails the test
// job instead of hanging it.
const testTimeout = 10 * time.Second

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

func TestOriginLifecycle(t *testing.T) {
	origin := moq.NewOriginProducer()
	_ = origin.Consume()
	origin.Dynamic().Cancel()
}

func TestDynamicBroadcastRequest(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	origin := moq.NewOriginProducer()
	dynamic := origin.Dynamic()
	defer dynamic.Cancel()

	type result struct {
		broadcast *moq.BroadcastConsumer
		err       error
	}
	requested := make(chan result, 1)
	go func() {
		broadcast, err := origin.Consume().RequestBroadcast("dynamic/broadcast")
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

	served, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	track, err := served.PublishTrack("status", nil)
	if err != nil {
		t.Fatal(err)
	}
	if err := request.Accept(served); err != nil {
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

	trackConsumer, err := res.broadcast.SubscribeTrack("status", nil)
	if err != nil {
		t.Fatal(err)
	}
	defer trackConsumer.Cancel()

	payload := []byte("served dynamically")
	if err := track.WriteFrame(payload, 0); err != nil {
		t.Fatal(err)
	}
	frame, err := trackConsumer.ReadFrame(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if frame == nil || string(frame.Payload) != string(payload) || frame.TimestampUs != 0 {
		t.Fatalf("frame = %+v, want payload=%q ts=0", frame, payload)
	}

	if err := track.Finish(); err != nil {
		t.Fatal(err)
	}
	if err := served.Finish(); err != nil {
		t.Fatal(err)
	}
}

func TestPublishMediaLifecycle(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	media, err := broadcast.PublishMedia("opus", opusHead())
	if err != nil {
		t.Fatal(err)
	}
	if err := media.WriteFrame([]byte("opus frame"), 1000); err != nil {
		t.Fatal(err)
	}
	if err := media.Finish(); err != nil {
		t.Fatal(err)
	}
	if err := broadcast.Finish(); err != nil {
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
	track, err := broadcast.PublishTrack("events", nil)
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
	if err := cached.WriteFrame([]byte("cached"), 0); err != nil {
		t.Fatal(err)
	}
	if err := cached.Finish(); err != nil {
		t.Fatal(err)
	}

	fetched, err := consumer.FetchGroup("events", 0, &moq.FetchGroupOptions{Priority: 3})
	if err != nil {
		t.Fatal(err)
	}
	frame, err := fetched.ReadFrame(ctx)
	if err != nil || frame == nil || string(frame.Payload) != "cached" {
		t.Fatalf("cached fetch: frame=%+v err=%v", frame, err)
	}

	dynamic, err := track.Dynamic()
	if err != nil {
		t.Fatal(err)
	}
	type fetchResult struct {
		group *moq.GroupConsumer
		err   error
	}
	result := make(chan fetchResult, 1)
	go func() {
		group, err := consumer.FetchGroup("events", 7, &moq.FetchGroupOptions{Priority: 11})
		result <- fetchResult{group: group, err: err}
	}()

	request, err := dynamic.RequestedGroup(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if request.Sequence() != 7 || request.Priority() != 11 {
		t.Fatalf("unexpected request: sequence=%d priority=%d", request.Sequence(), request.Priority())
	}
	produced, err := request.Accept()
	if err != nil {
		t.Fatal(err)
	}
	if err := produced.WriteFrame([]byte("archive"), request.Sequence()*20_000); err != nil {
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
	if _, err := broadcast.PublishMedia("nope", nil); err == nil {
		t.Fatal("expected error for unknown format")
	}
}

func TestLocalPublishConsumeAudio(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	origin := moq.NewOriginProducer()
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	media, err := broadcast.PublishMedia("opus", opusHead())
	if err != nil {
		t.Fatal(err)
	}
	if err := origin.Announce("live", broadcast); err != nil {
		t.Fatal(err)
	}

	consumer := origin.Consume()
	announced, err := consumer.Announced("")
	if err != nil {
		t.Fatal(err)
	}
	defer announced.Cancel()

	ann, err := announced.Next(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if ann == nil {
		t.Fatal("expected an announcement")
	}
	if ann.Path() != "live" {
		t.Fatalf("path = %q, want %q", ann.Path(), "live")
	}
	if hops := ann.Hops(); len(hops) != 0 {
		t.Fatalf("hops = %v, want empty for local origin", hops)
	}

	catalog, err := ann.Broadcast().Catalog(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(catalog.Audio) != 1 || len(catalog.Video) != 0 {
		t.Fatalf("catalog audio=%d video=%d, want 1/0", len(catalog.Audio), len(catalog.Video))
	}

	var trackName string
	var audio moq.Audio
	for name, a := range catalog.Audio {
		trackName, audio = name, a
	}
	if audio.Codec != "opus" || audio.SampleRate != 48000 || audio.ChannelCount != 2 {
		t.Fatalf("audio = %+v, want opus/48000/2", audio)
	}

	mediaConsumer, err := ann.Broadcast().SubscribeMedia(trackName, audio.Container, 10_000, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer mediaConsumer.Cancel()

	payload := []byte("opus audio payload data")
	if err := media.WriteFrame(payload, 1_000_000); err != nil {
		t.Fatal(err)
	}

	frame, err := mediaConsumer.Next(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if frame == nil {
		t.Fatal("expected a frame")
	}
	if string(frame.Payload) != string(payload) || frame.TimestampUs != 1_000_000 {
		t.Fatalf("frame = %+v, want payload=%q ts=1000000", frame, payload)
	}
}

func TestTrackPublishConsume(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	track, err := broadcast.PublishTrack("data", nil)
	if err != nil {
		t.Fatal(err)
	}
	consumer, err := track.Consume(nil)
	if err != nil {
		t.Fatal(err)
	}
	defer consumer.Cancel()

	if err := track.WriteFrame([]byte("hello"), 12_345); err != nil {
		t.Fatal(err)
	}

	frame, err := consumer.ReadFrame(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if frame == nil {
		t.Fatal("expected a frame")
	}
	if string(frame.Payload) != "hello" || frame.TimestampUs != 12_345 {
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
	if err := group.WriteFrame([]byte("group"), 23_456); err != nil {
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
	if string(frame.Payload) != "group" || frame.TimestampUs != 23_456 {
		t.Fatalf("frame = %+v, want payload=group ts=23456", frame)
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

func TestJSONTracks(t *testing.T) {
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

	snapshotOptions := moq.JSONSnapshotOptions{DeltaRatio: 8, Compression: true}
	snapshot, err := broadcast.PublishJSONSnapshot("status", snapshotOptions)
	if err != nil {
		t.Fatal(err)
	}
	snapshotConsumer, err := consumer.SubscribeJSONSnapshot("status", snapshotOptions)
	if err != nil {
		t.Fatal(err)
	}
	defer snapshotConsumer.Cancel()
	if err := snapshot.Update(map[string]any{"viewers": 42}); err != nil {
		t.Fatal(err)
	}
	value, err := snapshotConsumer.Next(ctx)
	if err != nil {
		t.Fatal(err)
	}
	var decoded map[string]any
	if err := json.Unmarshal(*value, &decoded); err != nil {
		t.Fatal(err)
	}
	if decoded["viewers"] != float64(42) {
		t.Fatalf("snapshot = %s", *value)
	}

	streamOptions := moq.JSONStreamOptions{Compression: true}
	stream, err := broadcast.PublishJSONStream("events", streamOptions)
	if err != nil {
		t.Fatal(err)
	}
	streamConsumer, err := consumer.SubscribeJSONStream("events", streamOptions)
	if err != nil {
		t.Fatal(err)
	}
	defer streamConsumer.Cancel()
	if err := stream.Append(map[string]any{"n": 1}); err != nil {
		t.Fatal(err)
	}
	record, err := streamConsumer.Next(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if string(*record) != `{"n":1}` {
		t.Fatalf("record = %s", *record)
	}

	if err := snapshot.Finish(); err != nil {
		t.Fatal(err)
	}
	if err := stream.Finish(); err != nil {
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
	defer broadcast.Finish()

	dynamic, err := broadcast.Dynamic()
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
		track, err := consumer.SubscribeTrack("events", nil)
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

	track, err := request.Accept(nil)
	if err != nil {
		t.Fatal(err)
	}
	payload := []byte("hello dynamic track")
	if err := track.WriteFrame(payload, 0); err != nil {
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
	if frame == nil || string(frame.Payload) != string(payload) || frame.TimestampUs != 0 {
		t.Fatalf("frame = %+v, want payload=%q ts=0", frame, payload)
	}
	if err := track.Finish(); err != nil {
		t.Fatal(err)
	}
}

func TestDynamicTrackRequestCanPublishMedia(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Finish()

	dynamic, err := broadcast.Dynamic()
	if err != nil {
		t.Fatal(err)
	}
	defer dynamic.Cancel()

	consumer, err := broadcast.Consume()
	if err != nil {
		t.Fatal(err)
	}

	type subscribeResult struct {
		media *moq.MediaConsumer
		err   error
	}
	subscribe := make(chan subscribeResult, 1)
	go func() {
		media, err := consumer.SubscribeMedia("requested-audio", moq.LegacyContainer(), 10_000, nil)
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

	media, err := broadcast.PublishMediaOnTrack(request, "opus", opusHead())
	if err != nil {
		t.Fatal(err)
	}
	mediaName, err := media.Name()
	if err != nil {
		t.Fatal(err)
	}
	if mediaName != "requested-audio" {
		t.Fatalf("media name = %q, want requested-audio", mediaName)
	}
	if _, err := request.Name(); !errors.Is(err, moq.ErrClosed) {
		t.Fatalf("request name after accept error = %v, want ErrClosed", err)
	}

	var mediaConsumer *moq.MediaConsumer
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
	if err := media.WriteFrame(payload, 20_000); err != nil {
		t.Fatal(err)
	}

	frame, err := mediaConsumer.Next(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if frame == nil {
		t.Fatal("expected a frame")
	}
	if string(frame.Payload) != string(payload) || frame.TimestampUs != 20_000 {
		t.Fatalf("frame = %+v, want payload=%q ts=20000", frame, payload)
	}
	if err := media.Finish(); err != nil {
		t.Fatal(err)
	}
}

// TestRecvGroupCancelRace exercises the core runCancellable path under -race:
// the native RecvGroup runs on an internal goroutine while ctx expiry triggers a
// concurrent Cancel on the same consumer. No group is ever written, so each read
// blocks until its short ctx fires. The race detector flags any unsynchronized
// access between the in-flight call and the cancel.
func TestRecvGroupCancelRace(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Finish()

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
	defer broadcast.Finish()

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
