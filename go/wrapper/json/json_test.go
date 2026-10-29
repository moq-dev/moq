package json_test

import (
	"context"
	"encoding/json"
	"errors"
	"reflect"
	"testing"
	"time"

	"moq.dev/moq"
	moqjson "moq.dev/moq/json"
)

// testTimeout bounds the blocking stream calls so a regression fails the test
// job instead of hanging it.
const testTimeout = 10 * time.Second

// newBroadcast returns a standalone broadcast and a consumer reading it.
func newBroadcast(t *testing.T) (*moq.BroadcastProducer, *moq.BroadcastConsumer) {
	t.Helper()
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	consumer, err := broadcast.Consume()
	if err != nil {
		t.Fatal(err)
	}
	return broadcast, consumer
}

// publishTrack creates a raw track for a JSON producer to take over.
func publishTrack(t *testing.T, broadcast *moq.BroadcastProducer, name string) *moq.TrackProducer {
	t.Helper()
	track, err := broadcast.PublishTrack(name, nil)
	if err != nil {
		t.Fatal(err)
	}
	return track
}

// subscribeTrack subscribes to a raw track for a JSON consumer to take over.
func subscribeTrack(ctx context.Context, t *testing.T, consumer *moq.BroadcastConsumer, name string) *moq.TrackConsumer {
	t.Helper()
	track, err := consumer.SubscribeTrack(ctx, name, nil)
	if err != nil {
		t.Fatal(err)
	}
	return track
}

func TestTracks(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()
	broadcast, consumer := newBroadcast(t)

	scale := uint64(1_000_000)
	track, err := broadcast.PublishTrack("status", &moq.TrackInfo{Timescale: &scale, Compress: true})
	if err != nil {
		t.Fatal(err)
	}
	snapshot, err := moqjson.NewSnapshotProducer(broadcast, track, moqjson.SnapshotOptions{})
	if err != nil {
		t.Fatal(err)
	}
	snapshotConsumer, err := moqjson.NewSnapshotConsumer(subscribeTrack(ctx, t, consumer, "status"))
	if err != nil {
		t.Fatal(err)
	}
	defer snapshotConsumer.Cancel()
	for viewers := 1; viewers <= 4; viewers++ {
		at := time.Duration(viewers) * time.Millisecond
		if err := snapshot.Update(map[string]any{"viewers": viewers}, &at); err != nil {
			t.Fatal(err)
		}
	}

	// Next yields every state in order; Latest skips the buffered third.
	expect := func(value *moqjson.Timed, err error, viewers int) {
		t.Helper()
		if err != nil {
			t.Fatal(err)
		}
		var decoded map[string]any
		if err := json.Unmarshal(value.Value, &decoded); err != nil {
			t.Fatal(err)
		}
		if decoded["viewers"] != float64(viewers) {
			t.Fatalf("snapshot = %s, want viewers %d", value.Value, viewers)
		}
		if want := time.Duration(viewers) * time.Millisecond; value.Timestamp == nil || *value.Timestamp != want {
			t.Fatalf("timestamp = %v, want %v", value.Timestamp, want)
		}
	}
	value, err := snapshotConsumer.Next(ctx)
	expect(value, err, 1)
	value, err = snapshotConsumer.Next(ctx)
	expect(value, err, 2)
	value, err = snapshotConsumer.Latest(ctx)
	expect(value, err, 4)

	stream, err := moqjson.NewStreamProducer(broadcast, publishTrack(t, broadcast, "events"))
	if err != nil {
		t.Fatal(err)
	}
	streamConsumer, err := moqjson.NewStreamConsumer(subscribeTrack(ctx, t, consumer, "events"))
	if err != nil {
		t.Fatal(err)
	}
	defer streamConsumer.Cancel()
	if err := stream.Append(map[string]any{"n": 1}, nil); err != nil {
		t.Fatal(err)
	}
	record, err := streamConsumer.Next(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if string(record.Value) != `{"n":1}` || record.Timestamp != nil {
		t.Fatalf("record = %s at %v, want untimed", record.Value, record.Timestamp)
	}

	if err := snapshot.Finish(); err != nil {
		t.Fatal(err)
	}
	if err := stream.Finish(); err != nil {
		t.Fatal(err)
	}
}

// A window reader sees each push and pop in order, with its timestamp.
func TestWindow(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()
	broadcast, consumer := newBroadcast(t)

	scale := uint64(1_000_000)
	track, err := broadcast.PublishTrack("log", &moq.TrackInfo{Timescale: &scale, Compress: true})
	if err != nil {
		t.Fatal(err)
	}
	window, err := moqjson.NewWindowProducer(track, moqjson.WindowOptions{})
	if err != nil {
		t.Fatal(err)
	}
	windowConsumer, err := moqjson.NewWindowConsumer(subscribeTrack(ctx, t, consumer, "log"))
	if err != nil {
		t.Fatal(err)
	}
	defer windowConsumer.Cancel()

	ms := func(n int) *time.Duration {
		at := time.Duration(n) * time.Millisecond
		return &at
	}
	for n := range 3 {
		if err := window.Push(map[string]any{"n": n}, ms(n)); err != nil {
			t.Fatal(err)
		}
	}
	if err := window.Pop(2, ms(3)); err != nil {
		t.Fatal(err)
	}

	want := []moqjson.TimedWindowEvent{
		{Event: moqjson.WindowPush{Index: 0, Value: json.RawMessage(`{"n":0}`)}, Timestamp: ms(0)},
		{Event: moqjson.WindowPush{Index: 1, Value: json.RawMessage(`{"n":1}`)}, Timestamp: ms(1)},
		{Event: moqjson.WindowPush{Index: 2, Value: json.RawMessage(`{"n":2}`)}, Timestamp: ms(2)},
		{Event: moqjson.WindowPop{Start: 0, End: 2}, Timestamp: ms(3)},
	}
	for _, expected := range want {
		event, err := windowConsumer.Next(ctx)
		if err != nil {
			t.Fatal(err)
		}
		if event == nil || !reflect.DeepEqual(*event, expected) {
			t.Fatalf("event = %+v, want %+v", event, expected)
		}
	}

	// The cut's new group restates record 2, which this reader already has.
	if err := window.Cut(); err != nil {
		t.Fatal(err)
	}
	if err := window.Push(map[string]any{"n": 3}, ms(4)); err != nil {
		t.Fatal(err)
	}
	if err := window.Finish(); err != nil {
		t.Fatal(err)
	}
	var got []moqjson.TimedWindowEvent
	for event, err := range windowConsumer.All(ctx) {
		if err != nil {
			t.Fatal(err)
		}
		got = append(got, *event)
	}
	tail := []moqjson.TimedWindowEvent{
		{Event: moqjson.WindowPush{Index: 3, Value: json.RawMessage(`{"n":3}`)}, Timestamp: ms(4)},
	}
	if !reflect.DeepEqual(got, tail) {
		t.Fatalf("events after the cut = %+v, want %+v", got, tail)
	}
}

// A producer takes over its track, so the raw handle is closed afterward.
func TestProducerTakesTheTrack(t *testing.T) {
	broadcast, _ := newBroadcast(t)
	track := publishTrack(t, broadcast, "status")
	snapshot, err := moqjson.NewSnapshotProducer(broadcast, track, moqjson.SnapshotOptions{})
	if err != nil {
		t.Fatal(err)
	}
	defer snapshot.Finish()
	if _, err := track.Demand(); !errors.Is(err, moq.ErrClosed) {
		t.Fatalf("Demand after take = %v, want ErrClosed", err)
	}
}

func TestDemand(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()
	broadcast, consumer := newBroadcast(t)

	snapshot, err := moqjson.NewSnapshotProducer(broadcast, publishTrack(t, broadcast, "status"), moqjson.SnapshotOptions{})
	if err != nil {
		t.Fatal(err)
	}
	stream, err := moqjson.NewStreamProducer(broadcast, publishTrack(t, broadcast, "events"))
	if err != nil {
		t.Fatal(err)
	}
	snapshotDemand, err := snapshot.Demand()
	if err != nil {
		t.Fatal(err)
	}
	streamDemand, err := stream.Demand()
	if err != nil {
		t.Fatal(err)
	}
	if name := snapshotDemand.Name(); name != "status" {
		t.Fatalf("Name = %q, want status", name)
	}
	if wanted := snapshotDemand.Wanted(); wanted != nil {
		t.Fatalf("Wanted = %d before any subscriber", *wanted)
	}

	snapshotConsumer, err := moqjson.NewSnapshotConsumer(subscribeTrack(ctx, t, consumer, "status"))
	if err != nil {
		t.Fatal(err)
	}
	streamConsumer, err := moqjson.NewStreamConsumer(subscribeTrack(ctx, t, consumer, "events"))
	if err != nil {
		t.Fatal(err)
	}
	if err := untilWanted(ctx, snapshotDemand, true); err != nil {
		t.Fatal(err)
	}
	if err := untilWanted(ctx, streamDemand, true); err != nil {
		t.Fatal(err)
	}

	snapshotConsumer.Cancel()
	streamConsumer.Cancel()
	if err := untilWanted(ctx, snapshotDemand, false); err != nil {
		t.Fatal(err)
	}
	if err := untilWanted(ctx, streamDemand, false); err != nil {
		t.Fatal(err)
	}

	if err := snapshot.Finish(); err != nil {
		t.Fatal(err)
	}
	if err := untilWanted(ctx, snapshotDemand, true); !errors.Is(err, moq.ErrClosed) {
		t.Fatalf("untilWanted after Finish = %v, want ErrClosed", err)
	}
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
