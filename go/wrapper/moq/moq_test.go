package moq_test

import (
	"context"
	"encoding/binary"
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
	if err := origin.Publish("live", broadcast); err != nil {
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

	mediaConsumer, err := ann.Broadcast().SubscribeMedia(trackName, audio.Container, 10_000)
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
	track, err := broadcast.PublishTrack("data")
	if err != nil {
		t.Fatal(err)
	}
	consumer, err := track.Consume()
	if err != nil {
		t.Fatal(err)
	}
	defer consumer.Cancel()

	if err := track.WriteFrame([]byte("hello")); err != nil {
		t.Fatal(err)
	}

	frame, err := consumer.ReadFrame(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if string(frame) != "hello" {
		t.Fatalf("frame = %q, want %q", frame, "hello")
	}
}
