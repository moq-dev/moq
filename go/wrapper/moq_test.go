package moq_test

import (
	"context"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"sync"
	"testing"
	"time"

	"moq.dev/moq"
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
		broadcast, err := origin.Consume().RequestBroadcast(ctx, "dynamic/broadcast")
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

	trackConsumer, err := res.broadcast.SubscribeTrack(ctx, "status", nil)
	if err != nil {
		t.Fatal(err)
	}
	defer trackConsumer.Cancel()

	payload := []byte("served dynamically")
	if err := track.WriteFrame(moq.Frame{Payload: payload, TimestampUs: 0}); err != nil {
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

func TestPublishAudioLifecycle(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	media, err := broadcast.PublishAudio(moq.AudioFormatOpus, opusHead())
	if err != nil {
		t.Fatal(err)
	}
	if err := media.WriteFrame(moq.Frame{Payload: []byte("opus frame"), TimestampUs: 1000}); err != nil {
		t.Fatal(err)
	}
	if err := media.Finish(); err != nil {
		t.Fatal(err)
	}
	if err := broadcast.Finish(); err != nil {
		t.Fatal(err)
	}
}

func TestVideoPropertiesUseDefaultedFields(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	rotation := 315.0
	if err := broadcast.SetVideoProperties(moq.VideoProperties{Rotation: &rotation}); err != nil {
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
	if err := cached.WriteFrame(moq.Frame{Payload: []byte("cached"), TimestampUs: 0}); err != nil {
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
		group, err := consumer.FetchGroup(ctx, "events", 7, &moq.FetchGroupOptions{Priority: 11})
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
	if err := produced.WriteFrame(moq.Frame{Payload: []byte("archive"), TimestampUs: request.Sequence()*20_000}); err != nil {
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
	if _, err := broadcast.PublishAudio(moq.AudioFormatOpus, nil); err == nil {
		t.Fatal("expected error for unknown format")
	}
}

func TestLocalPublishConsumeAudio(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()

	origin := moq.NewOriginProducer()
	broadcast, err := origin.CreateBroadcast("live")
	if err != nil {
		t.Fatal(err)
	}
	media, err := broadcast.PublishAudio(moq.AudioFormatOpus, opusHead())
	if err != nil {
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
	if !ann.Active() {
		t.Fatal("expected an active announcement")
	}
	if route := ann.Route(); len(route.Hops) != 0 {
		t.Fatalf("route hops = %v, want empty for local origin", route.Hops)
	}

	bc, err := consumer.RequestBroadcast(ctx, ann.Path())
	if err != nil {
		t.Fatal(err)
	}

	catalog, err := bc.Catalog(ctx)
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

	mediaConsumer, err := bc.SubscribeMedia(ctx, trackName, audio.Container, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer mediaConsumer.Cancel()

	payload := []byte("opus audio payload data")
	if err := media.WriteFrame(moq.Frame{Payload: payload, TimestampUs: 1_000_000}); err != nil {
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

	if err := track.WriteFrame(moq.Frame{Payload: []byte("hello"), TimestampUs: 12_345}); err != nil {
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
	if err := group.WriteFrame(moq.Frame{Payload: []byte("group"), TimestampUs: 23_456}); err != nil {
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

	snapshot, err := broadcast.PublishJSONSnapshot("status", moq.JSONSnapshotOptions{Compression: true})
	if err != nil {
		t.Fatal(err)
	}
	snapshotConsumer, err := consumer.SubscribeJSONSnapshot(ctx, "status", moq.JSONSubscribeOptions{Compression: true})
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

	stream, err := broadcast.PublishJSONStream("events", moq.JSONStreamOptions{Compression: true})
	if err != nil {
		t.Fatal(err)
	}
	streamConsumer, err := consumer.SubscribeJSONStream(ctx, "events", moq.JSONSubscribeOptions{Compression: true})
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

	track, err := request.Accept(nil)
	if err != nil {
		t.Fatal(err)
	}
	payload := []byte("hello dynamic track")
	if err := track.WriteFrame(moq.Frame{Payload: payload, TimestampUs: 0}); err != nil {
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

func TestDynamicTrackRequestCanPublishAudio(t *testing.T) {
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
		media, err := consumer.SubscribeMedia(ctx, "requested-audio", moq.LegacyContainer(), nil)
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

	media, err := broadcast.PublishAudioOnTrack(request, moq.AudioFormatOpus, opusHead())
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
	if err := media.WriteFrame(moq.Frame{Payload: payload, TimestampUs: 20_000}); err != nil {
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

// TestRequestBroadcastCancelKeepsTheOrigin cancels a RequestBroadcast parked on a
// dynamic handler, then proves the origin still resolves: the cancel has to abort
// that one request rather than the consumer it was made on.
func TestRequestBroadcastCancelKeepsTheOrigin(t *testing.T) {
	origin := moq.NewOriginProducer()
	dynamic := origin.Dynamic()
	defer dynamic.Cancel()
	consumer := origin.Consume()

	waitCtx, waitCancel := context.WithTimeout(context.Background(), testTimeout)
	defer waitCancel()

	ctx, cancel := context.WithCancel(context.Background())
	requested := make(chan error, 1)
	go func() {
		_, err := consumer.RequestBroadcast(ctx, "never/served")
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
		t.Fatal("RequestBroadcast did not return after its context was cancelled")
	}
	_ = pending.Abort(0)

	// The same consumer resolves the next path, which it could not do if the
	// cancel had torn the origin down.
	served, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer served.Finish()

	resolved := make(chan error, 1)
	go func() {
		_, err := consumer.RequestBroadcast(waitCtx, "later/served")
		resolved <- err
	}()

	next, err := dynamic.RequestedBroadcast(waitCtx)
	if err != nil {
		t.Fatal(err)
	}
	if err := next.Accept(served); err != nil {
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

// TestSubscribeTrackCancelKeepsTheBroadcast cancels a SubscribeTrack parked on a
// dynamic producer that has not accepted the track, then subscribes again on the
// same broadcast consumer.
func TestSubscribeTrackCancelKeepsTheBroadcast(t *testing.T) {
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

// TestUsedCancelKeepsTheTrack cancels a producer-side Used wait, which has no
// object-wide cancel to fall back on, and confirms the track still publishes.
func TestUsedCancelKeepsTheTrack(t *testing.T) {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Finish()

	track, err := broadcast.PublishTrack("status", nil)
	if err != nil {
		t.Fatal(err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	if err := track.Used(ctx); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("Used error = %v, want context.DeadlineExceeded", err)
	}

	readCtx, readCancel := context.WithTimeout(context.Background(), testTimeout)
	defer readCancel()

	consumer, err := track.Consume(nil)
	if err != nil {
		t.Fatal(err)
	}
	defer consumer.Cancel()
	if err := track.Used(readCtx); err != nil {
		t.Fatal(err)
	}

	payload := []byte("still publishing")
	if err := track.WriteFrame(moq.Frame{Payload: payload, TimestampUs: 0}); err != nil {
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
