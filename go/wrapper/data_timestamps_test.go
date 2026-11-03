package moq_test

import (
	"context"
	"encoding/json"
	"errors"
	"testing"

	"moq.dev/moq"
	moqjson "moq.dev/moq/json"
)

// A 90 kHz tick has no exact microsecond count, so it survives only as a record.
var tick = moq.Timestamp{Value: 1, Scale: 90_000}

func ninetyKHz() *moq.TrackInfo {
	scale := uint64(90_000)
	return &moq.TrackInfo{Timescale: &scale}
}

// Every producer carries an exact timestamp, a present zero, or none at all, and
// the raw reader gets back exactly what was written.
func TestDataTimestamps(t *testing.T) {
	timestamps := []struct {
		name string
		at   *moq.Timestamp
	}{
		{"untimed", nil},
		{"zero", &moq.Timestamp{Value: 0, Scale: 90_000}},
		{"tick", &tick},
	}
	for _, mode := range []string{"frame", "json snapshot", "json stream"} {
		for _, timestamp := range timestamps {
			t.Run(mode+"/"+timestamp.name, func(t *testing.T) {
				ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
				defer cancel()
				broadcast, err := moq.NewBroadcastProducer()
				if err != nil {
					t.Fatal(err)
				}
				defer broadcast.Close()
				var info *moq.TrackInfo
				if timestamp.at != nil {
					info = ninetyKHz()
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
				payload := []byte("opaque payload")
				if err := publishData(mode, broadcast, track, payload, timestamp.at); err != nil {
					t.Fatal(err)
				}
				frame, err := consumer.ReadFrame(ctx)
				if err != nil {
					t.Fatal(err)
				}
				if frame == nil {
					t.Fatal("missing frame")
				}
				want := timestamp.at
				if (frame.Timestamp == nil) != (want == nil) || want != nil && *frame.Timestamp != *want {
					t.Fatalf("timestamp = %+v, want %+v", frame.Timestamp, want)
				}
				if mode == "json snapshot" || mode == "json stream" {
					var value map[string]int
					if err := json.Unmarshal(frame.Payload, &value); err != nil {
						t.Fatal(err)
					}
					if value["n"] != 1 {
						t.Fatalf("payload = %s", frame.Payload)
					}
				} else if string(frame.Payload) != string(payload) {
					t.Fatalf("payload = %q", frame.Payload)
				}
			})
		}
	}
}

// publishData writes one value at timestamp through the producer mode names, then finishes.
func publishData(mode string, broadcast *moq.BroadcastProducer, track *moq.TrackProducer, payload []byte, timestamp *moq.Timestamp) error {
	switch mode {
	case "frame":
		return track.WriteFrame(moq.Frame{Payload: payload, Timestamp: timestamp})
	case "json snapshot":
		producer, err := moqjson.NewSnapshotProducer(broadcast, track, moqjson.SnapshotOptions{})
		if err != nil {
			return err
		}
		if err := producer.Update(map[string]int{"n": 1}, timestamp); err != nil {
			return err
		}
		return producer.Finish()
	case "json stream":
		producer, err := moqjson.NewStreamProducer(broadcast, track)
		if err != nil {
			return err
		}
		if err := producer.Append(map[string]int{"n": 1}, timestamp); err != nil {
			return err
		}
		return producer.Finish()
	}
	panic("unknown mode " + mode)
}

// A zero scale, or a value or scale past what the wire carries, is refused rather
// than clamped, from a raw frame and a data producer alike.
func TestTimestampRefused(t *testing.T) {
	for _, bad := range []struct {
		name string
		at   moq.Timestamp
	}{
		{"zero scale", moq.Timestamp{Value: 1, Scale: 0}},
		{"scale 2^62", moq.Timestamp{Value: 1, Scale: 1 << 62}},
		{"value 2^61", moq.Timestamp{Value: 1 << 61, Scale: 90_000}},
	} {
		for _, mode := range []string{"frame", "json stream"} {
			t.Run(mode+"/"+bad.name, func(t *testing.T) {
				broadcast, err := moq.NewBroadcastProducer()
				if err != nil {
					t.Fatal(err)
				}
				defer broadcast.Close()
				track, err := broadcast.PublishTrack("data", ninetyKHz())
				if err != nil {
					t.Fatal(err)
				}
				if err := publishData(mode, broadcast, track, []byte("x"), &bad.at); !errors.Is(err, moq.ErrTimeOverflow) {
					t.Fatalf("err = %v, want ErrTimeOverflow", err)
				}
			})
		}
	}
}
