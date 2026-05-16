package moq

import (
	"context"
	"encoding/binary"
	"errors"
	"testing"
	"time"
)

// opusHead builds a valid OpusHead init buffer (RFC 7845).
func opusHead() []byte {
	buf := []byte("OpusHead")
	buf = append(buf, 1, 2)                            // version, channels
	buf = binary.LittleEndian.AppendUint16(buf, 0)     // pre-skip
	buf = binary.LittleEndian.AppendUint32(buf, 48000) // sample rate
	buf = binary.LittleEndian.AppendUint16(buf, 0)     // output gain
	buf = append(buf, 0)                               // channel mapping
	return buf
}

func TestOriginLifecycle(t *testing.T) {
	origin := NewOriginProducer()
	defer origin.Close()
	consumer := origin.Consume()
	consumer.Close()
}

func TestPublishMediaLifecycle(t *testing.T) {
	broadcast, err := NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()
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
	media.Close()
	if err := broadcast.Finish(); err != nil {
		t.Fatal(err)
	}
}

func TestUnknownFormat(t *testing.T) {
	broadcast, err := NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()
	if _, err := broadcast.PublishMedia("nope", nil); err == nil {
		t.Fatal("expected error for unknown format")
	}
}

func TestLocalPublishConsumeAudio(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	origin := NewOriginProducer()
	defer origin.Close()
	broadcast, err := NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()
	media, err := broadcast.PublishMedia("opus", opusHead())
	if err != nil {
		t.Fatal(err)
	}
	defer media.Close()

	if err := origin.Publish("live", broadcast); err != nil {
		t.Fatal(err)
	}

	consumer := origin.Consume()
	defer consumer.Close()

	announced, err := consumer.Announced("")
	if err != nil {
		t.Fatal(err)
	}
	defer announced.Close()

	ann, err := announced.Next(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if ann == nil {
		t.Fatal("no announcement")
	}
	defer ann.Close()
	if ann.Path() != "live" {
		t.Fatalf("path: %s", ann.Path())
	}

	bc := ann.Broadcast()
	defer bc.Close()

	cat, err := bc.Catalog(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if cat == nil {
		t.Fatal("no catalog")
	}
	if len(cat.Audio) != 1 || len(cat.Video) != 0 {
		t.Fatalf("catalog: %+v", cat)
	}

	var trackName string
	for k := range cat.Audio {
		trackName = k
	}
	audio := cat.Audio[trackName]
	if audio.Codec != "opus" || audio.SampleRate != 48000 || audio.ChannelCount != 2 {
		t.Fatalf("audio: %+v", audio)
	}

	mc, err := bc.SubscribeMedia(trackName, audio.Container, 10_000)
	if err != nil {
		t.Fatal(err)
	}
	defer mc.Close()

	payload := []byte("opus audio payload data")
	if err := media.WriteFrame(payload, 1_000_000); err != nil {
		t.Fatal(err)
	}

	frame, err := mc.Next(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if frame == nil {
		t.Fatal("no frame")
	}
	if string(frame.Payload) != string(payload) || frame.TimestampUs != 1_000_000 {
		t.Fatalf("frame: %+v", frame)
	}
}

func TestRawPublishConsume(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	broadcast, err := NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()

	track, err := broadcast.PublishTrack("events")
	if err != nil {
		t.Fatal(err)
	}
	defer track.Close()

	tc, err := track.Consume()
	if err != nil {
		t.Fatal(err)
	}
	defer tc.Close()

	if err := track.WriteFrame([]byte("hello")); err != nil {
		t.Fatal(err)
	}
	if err := track.WriteFrame([]byte("world")); err != nil {
		t.Fatal(err)
	}

	frame, err := tc.ReadFrame(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if string(frame) != "hello" {
		t.Fatalf("frame1: %s", frame)
	}
	frame, err = tc.ReadFrame(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if string(frame) != "world" {
		t.Fatalf("frame2: %s", frame)
	}
}

func TestAppendGroupSequence(t *testing.T) {
	broadcast, err := NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()

	track, err := broadcast.PublishTrack("seq")
	if err != nil {
		t.Fatal(err)
	}
	defer track.Close()

	for i := uint64(0); i < 5; i++ {
		g, err := track.AppendGroup()
		if err != nil {
			t.Fatal(err)
		}
		if g.Sequence() != i {
			t.Fatalf("seq %d: got %d", i, g.Sequence())
		}
		if err := g.Finish(); err != nil {
			t.Fatal(err)
		}
		g.Close()
	}
}

func TestReadFrameReturnsNilAfterFinish(t *testing.T) {
	broadcast, err := NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()
	track, err := broadcast.PublishTrack("done")
	if err != nil {
		t.Fatal(err)
	}
	defer track.Close()

	consumer, err := track.Consume()
	if err != nil {
		t.Fatal(err)
	}
	defer consumer.Close()

	if err := track.WriteFrame([]byte("only")); err != nil {
		t.Fatal(err)
	}
	if err := track.Finish(); err != nil {
		t.Fatal(err)
	}

	ctx := context.Background()
	frame, err := consumer.ReadFrame(ctx)
	if err != nil || string(frame) != "only" {
		t.Fatalf("first: %v %s", err, frame)
	}
	frame, err = consumer.ReadFrame(ctx)
	if err != nil {
		t.Fatalf("nil-after-finish err: %v", err)
	}
	if frame != nil {
		t.Fatalf("expected nil frame, got %s", frame)
	}
}

func TestContextCancellation(t *testing.T) {
	broadcast, err := NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer broadcast.Close()
	track, err := broadcast.PublishTrack("blocked")
	if err != nil {
		t.Fatal(err)
	}
	defer track.Close()
	consumer, err := track.Consume()
	if err != nil {
		t.Fatal(err)
	}
	defer consumer.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()

	_, err = consumer.ReadFrame(ctx)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("expected DeadlineExceeded, got %v", err)
	}
}
