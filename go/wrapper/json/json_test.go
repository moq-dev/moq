package json_test

import (
	"context"
	"encoding/json"
	"errors"
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

	snapshot, err := moqjson.NewSnapshotProducer(broadcast, publishTrack(t, broadcast, "status"), moqjson.SnapshotOptions{Compression: true})
	if err != nil {
		t.Fatal(err)
	}
	snapshotConsumer, err := moqjson.NewSnapshotConsumer(subscribeTrack(ctx, t, consumer, "status"), moqjson.ConsumerOptions{Compression: true})
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

	stream, err := moqjson.NewStreamProducer(broadcast, publishTrack(t, broadcast, "events"), moqjson.StreamOptions{Compression: true})
	if err != nil {
		t.Fatal(err)
	}
	streamConsumer, err := moqjson.NewStreamConsumer(subscribeTrack(ctx, t, consumer, "events"), moqjson.ConsumerOptions{Compression: true})
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

// A producer takes over its track, so the raw handle is closed afterward.
func TestProducerTakesTheTrack(t *testing.T) {
	broadcast, _ := newBroadcast(t)
	track := publishTrack(t, broadcast, "status")
	snapshot, err := moqjson.NewSnapshotProducer(broadcast, track, moqjson.SnapshotOptions{})
	if err != nil {
		t.Fatal(err)
	}
	defer snapshot.Finish()
	if _, err := track.Name(); !errors.Is(err, moq.ErrClosed) {
		t.Fatalf("Name after take = %v, want ErrClosed", err)
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
	stream, err := moqjson.NewStreamProducer(broadcast, publishTrack(t, broadcast, "events"), moqjson.StreamOptions{})
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
	if snapshotDemand.IsUsed() {
		t.Fatal("IsUsed before any subscriber")
	}

	snapshotConsumer, err := moqjson.NewSnapshotConsumer(subscribeTrack(ctx, t, consumer, "status"), moqjson.ConsumerOptions{})
	if err != nil {
		t.Fatal(err)
	}
	streamConsumer, err := moqjson.NewStreamConsumer(subscribeTrack(ctx, t, consumer, "events"), moqjson.ConsumerOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if err := snapshotDemand.Used(ctx); err != nil {
		t.Fatal(err)
	}
	if err := streamDemand.Used(ctx); err != nil {
		t.Fatal(err)
	}

	snapshotConsumer.Cancel()
	streamConsumer.Cancel()
	if err := snapshotDemand.Unused(ctx); err != nil {
		t.Fatal(err)
	}
	if err := streamDemand.Unused(ctx); err != nil {
		t.Fatal(err)
	}

	if err := snapshot.Finish(); err != nil {
		t.Fatal(err)
	}
	if err := snapshotDemand.Used(ctx); !errors.Is(err, moq.ErrClosed) {
		t.Fatalf("Used after Finish = %v, want ErrClosed", err)
	}
}
