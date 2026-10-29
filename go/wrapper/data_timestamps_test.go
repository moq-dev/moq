package moq_test

import (
	"context"
	"encoding/json"
	"testing"
	"time"

	"moq.dev/moq"
	moqjson "moq.dev/moq/json"
)

func TestDataTimestamps(t *testing.T) {
	for _, mode := range []string{"json snapshot", "json stream"} {
		for _, timed := range []bool{false, true} {
			name := mode + "/untimed"
			if timed {
				name = mode + "/timed"
			}
			t.Run(name, func(t *testing.T) {
				ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
				defer cancel()
				broadcast, err := moq.NewBroadcastProducer()
				if err != nil {
					t.Fatal(err)
				}
				defer broadcast.Close()
				var timestamp *time.Duration
				var info *moq.TrackInfo
				if timed {
					at := 123456 * time.Microsecond
					timestamp = &at
					info = timedInfo()
				}
				track, err := broadcast.PublishTrack("data", info)
				if err != nil {
					t.Fatal(err)
				}
				consumer, err := track.Consume(nil)
				if err != nil {
					t.Fatal(err)
				}
				defer consumer.Cancel()
				switch mode {
				case "json snapshot":
					producer, err := moqjson.NewSnapshotProducer(broadcast, track, moqjson.SnapshotOptions{})
					if err != nil {
						t.Fatal(err)
					}
					err = producer.Update(map[string]int{"n": 1}, timestamp)
					if err != nil {
						t.Fatal(err)
					}
					if err := producer.Finish(); err != nil {
						t.Fatal(err)
					}
				case "json stream":
					producer, err := moqjson.NewStreamProducer(broadcast, track)
					if err != nil {
						t.Fatal(err)
					}
					err = producer.Append(map[string]int{"n": 1}, timestamp)
					if err != nil {
						t.Fatal(err)
					}
					if err := producer.Finish(); err != nil {
						t.Fatal(err)
					}
				}
				frame, err := consumer.ReadFrame(ctx)
				if err != nil {
					t.Fatal(err)
				}
				if frame == nil {
					t.Fatal("missing frame")
				}
				if (frame.Timestamp == nil) != (timestamp == nil) || timestamp != nil && *frame.Timestamp != *timestamp {
					t.Fatalf("timestamp = %v, want %v", frame.Timestamp, timestamp)
				}
				var value map[string]int
				if err := json.Unmarshal(frame.Payload, &value); err != nil {
					t.Fatal(err)
				}
				if value["n"] != 1 {
					t.Fatalf("payload = %s", frame.Payload)
				}
			})
		}
	}
}
